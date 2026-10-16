import { basicSetup, EditorView } from 'codemirror';
import { vim, Vim, getCM } from '@replit/codemirror-vim';

let active;
Vim.defineEx('write', 'w', () => active?.save());
Vim.defineEx('wq', 'wq', () => active?.save());
Vim.defineEx('xit', 'x', () => active?.save());
Vim.defineEx('quit', 'q', () => active?.cancel());

window.limpidCreateEditor = (parent, initial, save, cancel) => {
  const view = new EditorView({
    doc: initial,
    extensions: [
      vim(),
      basicSetup,
      EditorView.theme({
        '&': {color: 'var(--ink)', backgroundColor: 'var(--surface)', fontSize: '15px'},
        '.cm-content': {fontFamily: 'var(--mono)', minHeight: '120px', padding: '10px 0'},
        '.cm-line': {padding: '0 14px', lineHeight: '1.7'},
        '.cm-cursor': {borderLeftColor: 'var(--teal)'},
        '.cm-gutters': {backgroundColor: 'var(--surface)', color: 'var(--faint)', border: 'none'},
        '.cm-activeLine': {backgroundColor: 'transparent'},
        '.cm-activeLineGutter': {backgroundColor: 'transparent'},
        '&.cm-focused': {outline: 'none'},
      }),
    ],
    parent,
  });
  const session = {
    save() {
      if (!active) return;
      active = null;
      const text = view.state.doc.toString();
      save(text);
      // Let the Ex command finish before tearing the view down.
      setTimeout(() => view.destroy(), 0);
    },
    cancel() {
      if (!active) return;
      active = null;
      cancel();
      setTimeout(() => view.destroy(), 0);
    },
  };
  active = session;
  parent.addEventListener('keydown', event => {
    if (event.ctrlKey && event.key === 'Enter') {
      event.preventDefault();
      event.stopPropagation();
      session.save();
    }
  }, true);
  view.focus();
  Vim.handleKey(getCM(view), 'i');
  return session;
};
