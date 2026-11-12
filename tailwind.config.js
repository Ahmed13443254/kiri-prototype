// Tailwind mapping for the kiri design tokens.
//
// Every colour, size and radius in the app comes from a CSS custom property
// (see tokens.css), never from a hex literal in a component. That is what
// makes the settings screen's theme switch and replica-brand's re-skin
// possible without touching component code: change the variable, the
// utility follows.
//
// Usage: <div class="bg-surface text-muted border-border rounded-md p-4">
//         <button class="bg-accent text-on-accent hover:bg-accent/90">

/** @type {import('tailwindcss').Config} */
/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  darkMode: ["selector", '[data-theme="dark"]'],
  theme: {
    extend: {
      colors: {
        bg: "var(--k-bg)",
        sunken: "var(--k-sunken)",
        surface: "var(--k-surface)",
        border: {
          DEFAULT: "var(--k-border)",
          strong: "var(--k-border-strong)",
          input: "var(--k-border-input)",
        },
        text: {
          DEFAULT: "var(--k-text)",
          muted: "var(--k-text-muted)",
          faint: "var(--k-text-faint)",
        },
        accent: {
          DEFAULT: "var(--k-accent)",
          soft: "var(--k-accent-soft)",
        },
        "on-accent": "var(--k-on-accent)",
        danger: {
          DEFAULT: "var(--k-danger)",
          soft: "var(--k-danger-soft)",
        },
        "on-danger": "var(--k-on-danger)",
        success: {
          DEFAULT: "var(--k-success)",
          soft: "var(--k-success-soft)",
        },
        "on-success": "var(--k-on-success)",
        warning: {
          DEFAULT: "var(--k-warning)",
          soft: "var(--k-warning-soft)",
        },
        "on-warning": "var(--k-on-warning)",
      },
      fontFamily: {
        sans: "var(--k-font-sans)",
        mono: "var(--k-font-mono)",
      },
      fontSize: {
        xs: ["var(--k-text-xs)", { lineHeight: "var(--k-leading-xs)" }],
        sm: ["var(--k-text-sm)", { lineHeight: "var(--k-leading-sm)" }],
        base: ["var(--k-text-base)", { lineHeight: "var(--k-leading-base)" }],
        lg: ["var(--k-text-lg)", { lineHeight: "var(--k-leading-lg)" }],
        xl: ["var(--k-text-xl)", { lineHeight: "var(--k-leading-xl)" }],
        "2xl": ["var(--k-text-2xl)", { lineHeight: "var(--k-leading-2xl)" }],
      },
      fontWeight: {
        regular: "var(--k-weight-regular)",
        medium: "var(--k-weight-medium)",
        semibold: "var(--k-weight-semibold)",
        bold: "var(--k-weight-bold)",
      },
      borderRadius: {
        sm: "var(--k-radius-sm)",
        md: "var(--k-radius-md)",
        lg: "var(--k-radius-lg)",
        pill: "var(--k-radius-pill)",
      },
      boxShadow: {
        card: "var(--k-shadow-card)",
        pop: "var(--k-shadow-pop)",
      },
    },
  },
  plugins: [],
};
