import localFont from 'next/font/local';

// The stylesheets name these families. The home page imports this module, so
// both files are preloaded there and are nearly always present for the first
// paint; if Bricolage is not, the text shows at once and changes face when it arrives.
// A font loader has to be assigned; nothing reads the constants, whose
// generated class names would not match these families.
const bricolage = localFont({
  src: '../fonts/bricolage-grotesque-latin.woff2',
  weight: '400 800',
  display: 'swap',
  adjustFontFallback: false,
  declarations: [{ prop: 'font-family', value: 'Bricolage' }],
});

const terminalSymbols = localFont({
  src: '../fonts/terminal-symbols.woff2',
  display: 'block',
  adjustFontFallback: false,
  declarations: [{ prop: 'font-family', value: "'Terminal Symbols'" }],
});
