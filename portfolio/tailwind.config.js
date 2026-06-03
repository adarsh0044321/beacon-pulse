/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        beacon: {
          light: '#c084fc',
          DEFAULT: '#a855f7',
          dark: '#7e22ce',
        },
        pulse: {
          light: '#22d3ee',
          DEFAULT: '#06b6d4',
          dark: '#0891b2',
        },
        darkbg: {
          900: '#030712',
          800: '#0f172a',
          700: '#1e293b',
          600: '#334155',
        }
      },
      fontFamily: {
        sans: ['Outfit', 'Inter', 'system-ui', '-apple-system', 'sans-serif'],
      },
      boxShadow: {
        'beacon-glow': '0 0 25px rgba(168, 85, 247, 0.35)',
        'pulse-glow': '0 0 25px rgba(6, 182, 212, 0.35)',
      }
    },
  },
  plugins: [],
}
