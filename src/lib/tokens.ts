export const tokens = {
  bg: {
    app: '#0a0c10',
    surface: '#0d1017',
    elevated: '#11151e',
    titlebar: '#07090d',
  },
  border: {
    subtle: '#1e2535',
    dim: '#2e3450',
    warm: '#3a2e10',
  },
  gold: {
    primary: '#c8a96e',
    dim: '#7a6820',
    bg: '#1a1508',
  },
  status: {
    ok: {
      text: '#4a9e6a',
      bg: '#0f2a18',
      border: '#1e4a2e',
    },
    warn: {
      text: '#c8832a',
      bg: '#1a0d08',
      border: '#4a2a18',
    },
    danger: {
      text: '#9e4a4a',
      bg: '#1a0808',
    },
  },
  accent: {
    green: '#4a9e6a',
    red:   '#c14a4a',
  },
  text: {
    primary: '#e8e4d9',
    secondary: '#c8c4b8',
    muted: '#b8b4a8',
    dim: '#8a8fa8',
    ghost: '#4a5068',
    code: '#7a9fbc',
  },
  spacing: {
    xs: '4px',
    sm: '8px',
    md: '12px',
    lg: '16px',
    xl: '20px',
    xxl: '24px',
  },
  radius: {
    sm: '4px',
    md: '6px',
    lg: '8px',
    xl: '12px',
    pill: '20px',
  },
  font: {
    ui: '"JetBrains Mono", monospace',
    brand: '"Cinzel", serif',
  }
} as const;

export type Tokens = typeof tokens;
