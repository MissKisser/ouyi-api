import { createContext } from 'react';

interface AuthContextType {
  token: string | null;
  login: (password: string) => Promise<{ success: boolean; error?: string }>;
  setup: (password: string) => Promise<{ success: boolean; error?: string }>;
  logout: () => void;
  isAuthenticated: boolean;
}

export const AuthContext = createContext<AuthContextType | null>(null);
export type { AuthContextType };
