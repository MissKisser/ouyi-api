[CmdletBinding()]
param(
    [string]$BaseUrl = 'https://ouyi.viaxv.cn',
    [string[]]$Models = @('glm-5.2', 'gpt-5.6-sol'),
    [string[]]$Protocols = @('openai-chat', 'anthropic', 'responses'),
    [bool[]]$StreamModes = @($false, $true),
    [int]$Repetitions = 5,
    [int]$TimeoutSeconds = 180,
    [string]$ApiKey = $env:OUYI_API_KEY
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$BaseUrl = $BaseUrl.TrimEnd('/')
if ([string]::IsNullOrWhiteSpace($ApiKey)) {
    $ApiKey = 'sk-smoke-test-placeholder'
}

$Prompt = @'
Complete this deterministic workflow. First call Read with path "alpha". After you receive
that tool result, call Bash with a command that calculates 7 multiplied by 6. Do not call both
tools in the same response. After Bash returns, answer with a final sentence containing FINAL:42.
Never invent a tool result and never skip either tool.
'@

$JsonSchemaReadFact = @{
    type = 'object'
    properties = @{ path = @{ type = 'string'; enum = @('alpha') } }
    required = @('path')
    additionalProperties = $false
}
$JsonSchemaCalculate = @{
    type = 'object'
    properties = @{ command = @{ type = 'string' } }
    required = @('command')
    additionalProperties = $false
}

$OpenAiTools = @(
    @{ type = 'function'; function = @{ name = 'Read'; description = 'Read the requested deterministic fact.'; parameters = $JsonSchemaReadFact } },
    @{ type = 'function'; function = @{ name = 'Bash'; description = 'Run the requested calculation command.'; parameters = $JsonSchemaCalculate } }
)
$AnthropicTools = @(
    @{ name = 'Read'; description = 'Read the requested deterministic fact.'; input_schema = $JsonSchemaReadFact },
    @{ name = 'Bash'; description = 'Run the requested calculation command.'; input_schema = $JsonSchemaCalculate }
)
$ResponsesTools = @(
    @{ type = 'function'; name = 'Read'; description = 'Read the requested deterministic fact.'; parameters = $JsonSchemaReadFact },
    @{ type = 'function'; name = 'Bash'; description = 'Run the requested calculation command.'; parameters = $JsonSchemaCalculate }
)

function Invoke-ProxyRequest {
    param(
        [string]$Path,
        [hashtable]$Body,
        [switch]$Anthropic
    )

    $requestPath = Join-Path $env:TEMP ("ouyi-probe-request-{0}.json" -f [guid]::NewGuid())
    $responsePath = Join-Path $env:TEMP ("ouyi-probe-response-{0}.txt" -f [guid]::NewGuid())
    $errorPath = Join-Path $env:TEMP ("ouyi-probe-error-{0}.txt" -f [guid]::NewGuid())
    try {
        $json = $Body | ConvertTo-Json -Depth 30 -Compress
        [IO.File]::WriteAllText($requestPath, $json, (New-Object Text.UTF8Encoding($false)))
        $arguments = @(
            '--silent', '--show-error', '--max-time', $TimeoutSeconds,
            '--output', $responsePath,
            '--write-out', '%{http_code}',
            '--header', 'Content-Type: application/json',
            '--header', ("Authorization: Bearer {0}" -f $ApiKey),
            '--data-binary', ("@{0}" -f $requestPath),
            ("{0}{1}" -f $BaseUrl, $Path)
        )
        if ($Anthropic) {
            $arguments = @(
                '--header', ("x-api-key: {0}" -f $ApiKey),
                '--header', 'anthropic-version: 2023-06-01'
            ) + $arguments
        }

        $statusText = (& curl.exe @arguments 2> $errorPath | Out-String).Trim()
        $curlExit = $LASTEXITCODE
        $responseText = if (Test-Path $responsePath) {
            [IO.File]::ReadAllText($responsePath)
        } else {
            ''
        }
        if ($curlExit -ne 0) {
            $curlError = if (Test-Path $errorPath) { [IO.File]::ReadAllText($errorPath).Trim() } else { '' }
            throw "curl failed ($curlExit): $curlError"
        }
        $status = 0
        if (-not [int]::TryParse($statusText, [ref]$status)) {
            throw "invalid HTTP status from curl: $statusText"
        }
        if ($status -lt 200 -or $status -ge 300) {
            $brief = $responseText.Substring(0, [Math]::Min(500, $responseText.Length))
            throw "HTTP $status from ${Path}: $brief"
        }
        return $responseText
    } finally {
        Remove-Item -LiteralPath $requestPath, $responsePath, $errorPath -Force -ErrorAction SilentlyContinue
    }
}

function Get-SseDataObjects {
    param([string]$Raw)
    $items = New-Object System.Collections.ArrayList
    foreach ($line in ($Raw -split "`r?`n")) {
        if (-not $line.StartsWith('data:')) { continue }
        $data = $line.Substring(5).Trim()
        if ([string]::IsNullOrWhiteSpace($data) -or $data -eq '[DONE]') { continue }
        try {
            [void]$items.Add(($data | ConvertFrom-Json))
        } catch {
            throw "invalid SSE JSON data: $data"
        }
    }
    return @($items)
}

function Assert-NoRawToolMarkup {
    param([string]$Raw)
    if ($Raw -match '<tool_call|<tool_use|tool▁calls') {
        throw 'raw tool markup leaked to the client'
    }
}

function Invoke-FakeTool {
    param([string]$Name, [object]$Arguments, [int]$ExpectedIndex)
    $toolSequence = @('Read', 'Bash')
    if ($ExpectedIndex -ge $toolSequence.Count -or $Name -ne $toolSequence[$ExpectedIndex]) {
        $expectedName = if ($ExpectedIndex -lt $toolSequence.Count) { $toolSequence[$ExpectedIndex] } else { '<none>' }
        throw "expected tool $expectedName (index=$ExpectedIndex), got $Name"
    }
    if ($Name -eq 'Read') {
        if ($Arguments.path -ne 'alpha') {
            throw "Read path must be alpha; got $($Arguments | ConvertTo-Json -Compress)"
        }
        return '7'
    }
    if ($Arguments.command -notmatch '7' -or $Arguments.command -notmatch '6') {
        throw "Bash command must calculate 7 and 6; got $($Arguments | ConvertTo-Json -Compress)"
    }
    return '42'
}

function Parse-OpenAiResponse {
    param([string]$Raw, [bool]$Stream)
    $calls = @{}
    $text = New-Object Text.StringBuilder
    if (-not $Stream) {
        $response = $Raw | ConvertFrom-Json
        $message = $response.choices[0].message
        if ($null -ne $message.content) { [void]$text.Append([string]$message.content) }
        foreach ($call in @($message.tool_calls)) {
            if ($null -eq $call) { continue }
            $calls[[string]$call.id] = [pscustomobject]@{
                id = [string]$call.id
                name = [string]$call.function.name
                arguments = [string]$call.function.arguments
            }
        }
    } else {
        foreach ($event in (Get-SseDataObjects $Raw)) {
            $delta = $event.choices[0].delta
            if ($null -ne $delta.content) { [void]$text.Append([string]$delta.content) }
            foreach ($call in @($delta.tool_calls)) {
                if ($null -eq $call) { continue }
                $index = if ($null -ne $call.index) { [int]$call.index } else { 0 }
                $key = [string]$index
                if (-not $calls.ContainsKey($key)) {
                    $calls[$key] = [pscustomobject]@{ id = ''; name = ''; arguments = '' }
                }
                if ($null -ne $call.id) { $calls[$key].id = [string]$call.id }
                if ($null -ne $call.function.name) { $calls[$key].name += [string]$call.function.name }
                if ($null -ne $call.function.arguments) { $calls[$key].arguments += [string]$call.function.arguments }
            }
        }
    }
    $ordered = @($calls.GetEnumerator() | Sort-Object Name | ForEach-Object { $_.Value })
    return [pscustomobject]@{ calls = $ordered; text = $text.ToString(); responseId = $null }
}

function Parse-AnthropicResponse {
    param([string]$Raw, [bool]$Stream)
    $blocks = @{}
    $text = New-Object Text.StringBuilder
    if (-not $Stream) {
        $response = $Raw | ConvertFrom-Json
        foreach ($block in @($response.content)) {
            if ($block.type -eq 'text') { [void]$text.Append([string]$block.text) }
            if ($block.type -eq 'tool_use') {
                $blocks[[string]$block.id] = [pscustomobject]@{
                    id = [string]$block.id
                    name = [string]$block.name
                    arguments = ($block.input | ConvertTo-Json -Depth 20 -Compress)
                }
            }
        }
    } else {
        foreach ($event in (Get-SseDataObjects $Raw)) {
            if ($event.type -eq 'content_block_start') {
                $block = $event.content_block
                $index = [string]$event.index
                if ($block.type -eq 'tool_use') {
                    $initial = if ($null -ne $block.input) { $block.input | ConvertTo-Json -Depth 20 -Compress } else { '' }
                    if ($initial -eq '{}') { $initial = '' }
                    $blocks[$index] = [pscustomobject]@{
                        id = [string]$block.id
                        name = [string]$block.name
                        arguments = [string]$initial
                    }
                }
            }
            if ($event.type -eq 'content_block_delta') {
                $index = [string]$event.index
                if ($event.delta.type -eq 'text_delta') { [void]$text.Append([string]$event.delta.text) }
                if ($event.delta.type -eq 'input_json_delta') {
                    if (-not $blocks.ContainsKey($index)) {
                        $blocks[$index] = [pscustomobject]@{ id = ''; name = ''; arguments = '' }
                    }
                    $blocks[$index].arguments += [string]$event.delta.partial_json
                }
            }
        }
    }
    $ordered = @($blocks.GetEnumerator() | Sort-Object Name | ForEach-Object { $_.Value })
    return [pscustomobject]@{ calls = $ordered; text = $text.ToString(); responseId = $null }
}

function Parse-ResponsesResponse {
    param([string]$Raw, [bool]$Stream)
    $response = $null
    if (-not $Stream) {
        $response = $Raw | ConvertFrom-Json
    } else {
        foreach ($event in (Get-SseDataObjects $Raw)) {
            if ($event.type -eq 'response.completed') { $response = $event.response }
        }
    }
    if ($null -eq $response) { throw 'Responses stream did not contain response.completed' }
    $calls = New-Object System.Collections.ArrayList
    $text = New-Object Text.StringBuilder
    foreach ($item in @($response.output)) {
        if ($item.type -eq 'function_call') {
            [void]$calls.Add([pscustomobject]@{
                id = [string]$item.call_id
                name = [string]$item.name
                arguments = [string]$item.arguments
            })
        }
        if ($item.type -eq 'message') {
            foreach ($part in @($item.content)) {
                if ($part.type -eq 'output_text') { [void]$text.Append([string]$part.text) }
            }
        }
    }
    return [pscustomobject]@{ calls = @($calls); text = $text.ToString(); responseId = [string]$response.id }
}

function Assert-FinalResult {
    param([object]$Parsed, [int]$ToolTurns)
    if ($Parsed.calls.Count -ne 0) { throw 'expected a final answer, got another tool call' }
    if ($ToolTurns -ne 2) { throw "expected two tool turns, completed $ToolTurns" }
    if ($Parsed.text -notmatch '42') { throw "final answer does not contain 42: $($Parsed.text)" }
}

function Invoke-OpenAiCase {
    param([string]$Model, [bool]$Stream)
    $messages = New-Object System.Collections.ArrayList
    [void]$messages.Add(@{ role = 'user'; content = $Prompt })
    $toolTurn = 0
    for ($round = 0; $round -lt 4; $round++) {
        $body = @{
            model = $Model; messages = @($messages); stream = $Stream; max_tokens = 256
        }
        if ($toolTurn -lt 2) {
            $body.tools = $OpenAiTools
            $body.tool_choice = 'required'
            $body.parallel_tool_calls = $false
        }
        $raw = Invoke-ProxyRequest -Path '/v1/chat/completions' -Body $body
        Assert-NoRawToolMarkup $raw
        $parsed = Parse-OpenAiResponse $raw $Stream
        if ($parsed.calls.Count -eq 0) { Assert-FinalResult $parsed $toolTurn; return }
        if ($parsed.calls.Count -ne 1) { throw "expected one sequential tool call, got $($parsed.calls.Count)" }
        $call = $parsed.calls[0]
        if ([string]::IsNullOrWhiteSpace($call.id)) { throw 'OpenAI tool call id is missing' }
        $arguments = $call.arguments | ConvertFrom-Json
        Write-Verbose ("OpenAI tool turn before execution: {0}" -f $toolTurn)
        $result = Invoke-FakeTool -Name $call.name -Arguments $arguments -ExpectedIndex $toolTurn
        [void]$messages.Add(@{
            role = 'assistant'; content = $null
            tool_calls = @(@{ id = $call.id; type = 'function'; function = @{ name = $call.name; arguments = $call.arguments } })
        })
        [void]$messages.Add(@{ role = 'tool'; tool_call_id = $call.id; name = $call.name; content = $result })
        $toolTurn++
    }
    throw 'OpenAI case exceeded the maximum number of rounds'
}

function Invoke-AnthropicCase {
    param([string]$Model, [bool]$Stream)
    $messages = New-Object System.Collections.ArrayList
    [void]$messages.Add(@{ role = 'user'; content = $Prompt })
    $toolTurn = 0
    for ($round = 0; $round -lt 4; $round++) {
        $body = @{
            model = $Model; messages = @($messages); stream = $Stream; max_tokens = 256
        }
        if ($toolTurn -lt 2) {
            $body.tools = $AnthropicTools
            $body.tool_choice = @{ type = 'any' }
        }
        $raw = Invoke-ProxyRequest -Path '/anthropic/v1/messages' -Body $body -Anthropic
        Assert-NoRawToolMarkup $raw
        $parsed = Parse-AnthropicResponse $raw $Stream
        if ($parsed.calls.Count -eq 0) { Assert-FinalResult $parsed $toolTurn; return }
        if ($parsed.calls.Count -ne 1) { throw "expected one sequential tool call, got $($parsed.calls.Count)" }
        $call = $parsed.calls[0]
        if ([string]::IsNullOrWhiteSpace($call.id)) { throw 'Anthropic tool_use id is missing' }
        $arguments = $call.arguments | ConvertFrom-Json
        $result = Invoke-FakeTool -Name $call.name -Arguments $arguments -ExpectedIndex $toolTurn
        [void]$messages.Add(@{
            role = 'assistant'; content = @(@{ type = 'tool_use'; id = $call.id; name = $call.name; input = $arguments })
        })
        [void]$messages.Add(@{
            role = 'user'; content = @(@{ type = 'tool_result'; tool_use_id = $call.id; content = $result; is_error = $false })
        })
        $toolTurn++
    }
    throw 'Anthropic case exceeded the maximum number of rounds'
}

function Invoke-ResponsesCase {
    param([string]$Model, [bool]$Stream)
    $input = $Prompt
    $previousResponseId = $null
    $toolTurn = 0
    for ($round = 0; $round -lt 4; $round++) {
        $body = @{
            model = $Model; input = $input; stream = $Stream; max_output_tokens = 256
        }
        if ($toolTurn -lt 2) {
            $body.tools = $ResponsesTools
            $body.tool_choice = 'required'
            $body.parallel_tool_calls = $false
        }
        if (-not [string]::IsNullOrWhiteSpace($previousResponseId)) {
            $body.previous_response_id = $previousResponseId
        }
        $raw = Invoke-ProxyRequest -Path '/v1/responses' -Body $body
        Assert-NoRawToolMarkup $raw
        $parsed = Parse-ResponsesResponse $raw $Stream
        if ($parsed.calls.Count -eq 0) { Assert-FinalResult $parsed $toolTurn; return }
        if ($parsed.calls.Count -ne 1) { throw "expected one sequential tool call, got $($parsed.calls.Count)" }
        $call = $parsed.calls[0]
        if ([string]::IsNullOrWhiteSpace($call.id)) { throw 'Responses function call id is missing' }
        if ([string]::IsNullOrWhiteSpace($parsed.responseId)) { throw 'Responses response id is missing' }
        $arguments = $call.arguments | ConvertFrom-Json
        $result = Invoke-FakeTool -Name $call.name -Arguments $arguments -ExpectedIndex $toolTurn
        $input = @(@{ type = 'function_call_output'; call_id = $call.id; output = $result })
        $previousResponseId = $parsed.responseId
        $toolTurn++
    }
    throw 'Responses case exceeded the maximum number of rounds'
}

$results = New-Object System.Collections.ArrayList
foreach ($model in $Models) {
    foreach ($protocol in $Protocols) {
        foreach ($stream in $StreamModes) {
            for ($attempt = 1; $attempt -le $Repetitions; $attempt++) {
                $label = '{0} {1} stream={2} attempt={3}' -f $model, $protocol, $stream, $attempt
                $started = Get-Date
                try {
                    switch ($protocol) {
                        'openai-chat' { Invoke-OpenAiCase $model $stream }
                        'anthropic' { Invoke-AnthropicCase $model $stream }
                        'responses' { Invoke-ResponsesCase $model $stream }
                    }
                    $elapsed = [int]((Get-Date) - $started).TotalMilliseconds
                    [void]$results.Add([pscustomobject]@{ model = $model; protocol = $protocol; stream = $stream; attempt = $attempt; success = $true; elapsed_ms = $elapsed; error = $null })
                    Write-Host ("PASS {0} ({1} ms)" -f $label, $elapsed) -ForegroundColor Green
                } catch {
                    $elapsed = [int]((Get-Date) - $started).TotalMilliseconds
                    [void]$results.Add([pscustomobject]@{ model = $model; protocol = $protocol; stream = $stream; attempt = $attempt; success = $false; elapsed_ms = $elapsed; error = $_.Exception.Message })
                    Write-Host ("FAIL {0}: {1}" -f $label, $_.Exception.Message) -ForegroundColor Red
                }
            }
        }
    }
}

$passed = @($results | Where-Object { $_.success }).Count
$failed = $results.Count - $passed
[pscustomobject]@{
    base_url = $BaseUrl
    total = $results.Count
    passed = $passed
    failed = $failed
    results = @($results)
} | ConvertTo-Json -Depth 8

if ($failed -ne 0) { exit 1 }
