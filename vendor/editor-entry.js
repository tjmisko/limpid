import { basicSetup, EditorView } from 'codemirror';
import { vim, Vim, getCM } from '@replit/codemirror-vim';

let active;
Vim.defineEx('write', 'w', () => active?.save());
Vim.defineEx('quit', 'q', () => active?.cancel());

window.limpidCreateEditor = (parent, initial, save, cancel) => {
  const view = new EditorView({
    doc: initial,
    extensions: [
      vim(),
      basicSetup,
      EditorView.theme({
        '&': {color: 'var(--ink)', backgroundColor: 'var(--surface)'},
        '.cm-content': {fontFamily: 'var(--mono)', minHeight: '96px'},
        '.cm-cursor': {borderLeftColor: 'var(--teal)'},
        '.cm-gutters': {backgroundColor: 'var(--surface)', color: 'var(--faint)', border: 'none'},
      }),
    ],
    parent,
  });
  const session = {
    save() { save(view.state.doc.toString()); view.destroy(); active = null; },
    cancel() { cancel(); view.destroy(); active = null; },
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
