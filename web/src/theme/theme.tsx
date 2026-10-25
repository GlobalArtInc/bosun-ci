import { Theme } from '@radix-ui/themes';
import { createContext, type ReactNode, useCallback, useContext, useEffect, useMemo, useState } from 'react';

type Appearance = 'light' | 'dark';

interface Themes {
  appearance: Appearance;
  toggle: () => void;
}

interface ThemeProviderProps {
  children: ReactNode;
}

const STORAGE_KEY = 'bosun.theme';

function stored(): Appearance | null {
  try {
    const value = window.localStorage.getItem(STORAGE_KEY);
    return value === 'light' || value === 'dark' ? value : null;
  } catch {
    return null;
  }
}

function preferred(): Appearance {
  return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
}

const ThemeContext = createContext<Themes | null>(null);

export function ThemeProvider({ children }: ThemeProviderProps) {
  const [appearance, setAppearance] = useState<Appearance>(() => stored() ?? preferred());

  useEffect(() => {
    document.documentElement.style.colorScheme = appearance;
  }, [appearance]);

  const toggle = useCallback(() => {
    setAppearance((current) => {
      const next = current === 'dark' ? 'light' : 'dark';
      try {
        window.localStorage.setItem(STORAGE_KEY, next);
      } catch {
        return next;
      }
      return next;
    });
  }, []);

  const value = useMemo<Themes>(() => ({ appearance, toggle }), [appearance, toggle]);

  return (
    <ThemeContext.Provider value={value}>
      <Theme appearance={appearance} accentColor="indigo" grayColor="slate" radius="medium">
        {children}
      </Theme>
    </ThemeContext.Provider>
  );
}

export function useTheme(): Themes {
  const value = useContext(ThemeContext);
  if (value === null) {
    throw new Error('useTheme is used outside ThemeProvider');
  }
  return value;
}
