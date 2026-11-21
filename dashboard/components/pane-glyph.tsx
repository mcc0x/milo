const paths = {
  outline: 'M6 3v14a2 2 0 0 0 2 2h10M6 8h12M6 13h8',
  history: 'M6 3v4m0 6v8m4-11h9m-9 5h6M9 10a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z',
  usage: 'M4 18v-5m6 5V5m6 13V9m4 12H3',
  appearance: 'M3 7h7m4 0h7M3 17h11m4 0h3M14 7a2 2 0 1 1-4 0 2 2 0 0 1 4 0Zm4 10a2 2 0 1 1-4 0 2 2 0 0 1 4 0Z',
};

export default function PaneGlyph({kind}: {kind: keyof typeof paths}) {
  return <svg className="pane-glyph" data-pane={kind} viewBox="0 0 24 24" fill="none" stroke="currentColor"
    strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d={paths[kind]}/></svg>;
}
