// Loaded only by the opt-in source server, never by the packaged editor.
(() => {
  let revision = document.currentScript.dataset.revision;
  const token = location.hash.slice(1);
  if (!token) return;
  const sectionKey = 'milo-dev-section';
  const section = sessionStorage.getItem(sectionKey);
  sessionStorage.removeItem(sectionKey);
  const editor = document.getElementById('editor');
  if (section && editor) {
    const restore = () => {
      if (editor.hidden) return;
      observer.disconnect();
      [...editor.querySelectorAll('[data-page]')].find(tab => tab.dataset.page === section)?.click();
    };
    const observer = new MutationObserver(restore);
    observer.observe(editor, {attributes: true, attributeFilter: ['hidden']});
    restore();
  }

  async function check() {
    if (!document.getElementById('closed')?.hidden) return;
    try {
      const response = await fetch('/dev/version', {headers: {Authorization: `Bearer ${token}`}, cache: 'no-store'});
      if (response.status === 401 || response.status === 403) return;
      if (response.ok) {
        const next = await response.text();
        if (next !== revision) {
          const sheet = document.querySelector('link[rel="stylesheet"][href="/app.css"]');
          if (sheet && next.slice(0, next.lastIndexOf('|')) === revision.slice(0, revision.lastIndexOf('|'))) {
            const replacement = sheet.cloneNode();
            await new Promise((resolve, reject) => {
              replacement.onload = () => { sheet.remove(); resolve(); };
              replacement.onerror = () => { replacement.remove(); reject(new Error('Stylesheet unavailable')); };
              sheet.after(replacement);
            });
            revision = next;
          } else if (window.dispatchEvent(new Event('milo:dev-reload', {cancelable: true}))) {
            const tab = document.querySelector('[data-page][aria-current="page"]');
            if (tab) sessionStorage.setItem(sectionKey, tab.dataset.page);
            location.reload();
            return;
          }
        }
      }
    } catch { /* An editor may briefly replace a file during a save. */ }
    setTimeout(check, 700);
  }
  setTimeout(check, 700);
})();
