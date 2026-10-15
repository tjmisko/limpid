(() => {
  const editorUrl = document.currentScript?.dataset.editor;
  const mermaidUrl = document.currentScript?.dataset.mermaid;
  const diagrams = [...document.querySelectorAll('pre.mermaid')];
  if (mermaidUrl && diagrams.length) requestAnimationFrame(() => setTimeout(() => {
    const script = document.createElement('script');
    script.src = new URL(mermaidUrl, location.href).href;
    script.onload = async () => {
      for (const [index, pre] of diagrams.entries()) {
        const source = pre.textContent;
        try {
          const svg = await window.limpidRenderMermaid(`limpid-mermaid-${index}`, source);
          const rendered = document.createElement('div');
          rendered.className = 'mermaid-rendered';
          rendered.innerHTML = svg;
          pre.replaceWith(rendered);
          window.webkit?.messageHandlers?.['mermaid-svg']?.postMessage({hash: pre.dataset.hash, source, svg});
        } catch { /* Keep the readable source if a diagram is invalid. */ }
      }
    };
    document.head.append(script);
  }, 0));
  let editorPromise;
  function loadEditor() {
    if (!editorUrl) return Promise.resolve(false);
    if (!editorPromise) editorPromise = new Promise(resolve => {
      const script = document.createElement('script');
      script.src = new URL(editorUrl, location.href).href;
      script.onload = () => resolve(!!window.limpidCreateEditor);
      script.onerror = () => resolve(false);
      document.head.append(script);
    });
    return editorPromise;
  }
  const theme = new URLSearchParams(location.search).get('theme');
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
  const links = [...document.querySelectorAll('.toc a')];
  const byId = new Map();
  for (const link of links) {
    const id = link.getAttribute('href').slice(1);
    if (!byId.has(id)) byId.set(id, []);
    byId.get(id).push(link);
  }
  if ('IntersectionObserver' in window) {
    const observer = new IntersectionObserver(entries => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        links.forEach(link => link.classList.remove('active'));
        (byId.get(entry.target.id) || []).forEach(link => link.classList.add('active'));
      }
    }, {rootMargin: '-15% 0px -75% 0px'});
    document.querySelectorAll('article h1[id],article h2[id],article h3[id]').forEach(head => observer.observe(head));
  }
  const fields = [...document.querySelectorAll('.question-field')];
  window.limpidCaptureAnchor = () => {
    const blocks = [...document.querySelectorAll('article [data-src]')];
    const block = blocks.filter(item => item.getBoundingClientRect().top <= 25).at(-1) || blocks[0];
    return block ? {src: block.dataset.src, turn: block.closest('.turn')?.id || '', offset: block.getBoundingClientRect().top} : null;
  };
  window.limpidRestoreAnchor = anchor => {
    if (!anchor) return;
    const block = [...document.querySelectorAll('article [data-src]')]
      .find(item => item.dataset.src === anchor.src && (item.closest('.turn')?.id || '') === anchor.turn);
    if (block) window.scrollTo({top: block.getBoundingClientRect().top + scrollY - anchor.offset});
  };
  window.limpidRestoreReplies = replies => {
    for (const [id, text] of Object.entries(replies)) {
      const field = document.getElementById(id);
      if (!field?.classList.contains('question-field')) continue;
      field.querySelector('.question-reply').textContent = text;
      for (const option of field.querySelectorAll('.question-option')) {
        option.classList.toggle('selected', option.textContent.trim() === text.trim());
      }
    }
  };
  window.limpidRestoreComments = comments => {
    for (const [key, comment] of Object.entries(comments)) {
      const [turn, range] = key.includes(':') ? key.split(':', 2) : ['', key];
      const target = [...document.querySelectorAll('article [data-src]')]
        .find(block => block.dataset.src === range && (block.closest('.turn')?.id || '') === turn);
      if (!target) continue;
      const note = document.createElement('aside');
      note.className = 'limpid-annotation';
      const heading = document.createElement('strong');
      heading.textContent = 'Comment';
      const text = document.createElement('p');
      text.textContent = comment.text;
      note.append(heading, text);
      target.after(note);
    }
  };
  const headings = [...document.querySelectorAll('article h1[id],article h2[id],article h3[id],article h4[id],article h5[id],article h6[id]')];
  let search = '';
  let searchBox;
  let zoom = 1;
  let pendingG = false;
  let editing = false;
  async function edit(target, kind) {
    if (editing || !target || !await loadEditor()) return;
    editing = true;
    const quote = target.textContent.trim().slice(0, 80);
    const mount = document.createElement('div');
    mount.className = 'limpid-editor';
    target.append(mount);
    const current = kind === 'reply' ? target.querySelector('.question-reply')?.textContent || '' : '';
    const finish = () => { mount.remove(); editing = false; target.focus?.(); };
    window.limpidCreateEditor(mount, current, text => {
      if (kind === 'reply') {
        target.querySelector('.question-reply').textContent = text;
        window.webkit?.messageHandlers?.reply?.postMessage({field: target.dataset.field, text, src: target.dataset.src});
      } else {
        const note = document.createElement('aside');
        note.className = 'limpid-annotation';
        note.textContent = `Comment: ${text}`;
        target.after(note);
        window.webkit?.messageHandlers?.comment?.postMessage({src: target.dataset.src, turn: target.closest('.turn')?.id || '', lineStart: Number(target.dataset.lineStart || 0), lineEnd: Number(target.dataset.lineEnd || 0), quote, text});
      }
      finish();
    }, finish);
  }
  function nearestBlock() {
    const blocks = [...document.querySelectorAll('article [data-src]')];
    const top = blocks.filter(block => block.getBoundingClientRect().top <= 28).at(-1);
    return top || blocks.find(block => block.getBoundingClientRect().top > 28);
  }
  window.limpidShowReview = message => {
    let dialog = document.querySelector('.limpid-review');
    if (!dialog) {
      dialog = document.createElement('dialog');
      dialog.className = 'limpid-review';
      const textarea = document.createElement('textarea');
      textarea.setAttribute('aria-label', 'Review reply');
      textarea.spellcheck = false;
      const hint = document.createElement('p');
      hint.textContent = 'Ctrl-Enter to send · Esc to close';
      dialog.append(textarea, hint);
      textarea.addEventListener('keydown', event => {
        if (event.ctrlKey && event.key === 'Enter') {
          event.preventDefault();
          window.webkit?.messageHandlers?.send?.postMessage({text: textarea.value});
          dialog.close();
        }
      });
      document.body.append(dialog);
    }
    dialog.querySelector('textarea').value = message;
    dialog.showModal();
    dialog.querySelector('textarea').focus();
  };
  function find(backwards = false) {
    if (!search) return;
    const smartcase = /[A-Z]/.test(search);
    window.find(search, smartcase, backwards, true);
  }
  function openSearch() {
    if (!searchBox) {
      searchBox = document.createElement('input');
      searchBox.className = 'limpid-search';
      searchBox.type = 'search';
      searchBox.setAttribute('aria-label', 'Search page');
      searchBox.placeholder = 'Search';
      searchBox.addEventListener('keydown', event => {
        if (event.key === 'Escape') {
          searchBox.hidden = true;
          searchBox.blur();
          event.preventDefault();
        } else if (event.key === 'Enter') {
          search = searchBox.value;
          find(event.shiftKey);
          searchBox.hidden = true;
          searchBox.blur();
          event.preventDefault();
        }
      });
      document.body.append(searchBox);
    }
    searchBox.hidden = false;
    searchBox.value = search;
    searchBox.focus();
    searchBox.select();
  }
  function editable() {
    const active = document.activeElement;
    return active && (active.matches('input,textarea,select') || active.isContentEditable);
  }
  document.addEventListener('keydown', event => {
    if (editable()) return;
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    if (event.key === 'g') {
      if (pendingG) {
        window.scrollTo({top: 0});
        pendingG = false;
      } else {
        pendingG = true;
        setTimeout(() => { pendingG = false; }, 650);
      }
      event.preventDefault();
      return;
    }
    pendingG = false;
    if (event.key === 'Tab' && fields.length) {
      event.preventDefault();
      const current = fields.indexOf(document.activeElement);
      const next = (current + (event.shiftKey ? -1 : 1) + fields.length) % fields.length;
      fields[next].focus();
      fields[next].scrollIntoView({block: 'center'});
      return;
    }
    const field = document.activeElement?.closest?.('.question-field');
    if (field && /^[1-9]$/.test(event.key)) {
      const options = [...field.querySelectorAll('.question-option')];
      const option = options[Number(event.key) - 1];
      if (!option) return;
      event.preventDefault();
      options.forEach(item => item.classList.remove('selected'));
      option.classList.add('selected');
      const answer = option.textContent.trim();
      field.querySelector('.question-reply').textContent = answer;
      window.webkit?.messageHandlers?.reply?.postMessage({field: field.dataset.field, text: answer, src: field.dataset.src});
      return;
    }
    if (field && (event.key === 'Enter' || event.key === 'i')) {
      event.preventDefault();
      edit(field, 'reply');
      return;
    }
    if (event.key === 'c') {
      event.preventDefault();
      edit(nearestBlock(), 'comment');
      return;
    }
    if (event.key === 'S') {
      event.preventDefault();
      window.webkit?.messageHandlers?.review?.postMessage({});
      return;
    }
    if (event.key === 't') {
      event.preventDefault();
      document.querySelector('.shell')?.classList.toggle('rail-hidden');
      return;
    }
    if ((event.key === '{' || event.key === '}') && headings.length) {
      event.preventDefault();
      const position = window.scrollY + 25;
      const anchors = headings.map(head => ({head, top: head.getBoundingClientRect().top + window.scrollY}));
      const target = event.key === '}'
        ? anchors.find(item => item.top > position + 2)
        : anchors.filter(item => item.top < position - 2).at(-1);
      target?.head.scrollIntoView({block: 'start'});
      return;
    }
    const scroll = {j: 80, k: -80, PageDown: innerHeight / 2, PageUp: -innerHeight / 2}[event.key];
    if (scroll !== undefined) {
      window.scrollBy({top: scroll, behavior: 'auto'});
      event.preventDefault();
      return;
    }
    if (event.key === 'G') { window.scrollTo({top: document.body.scrollHeight}); event.preventDefault(); return; }
    if (event.key === 'q') { window.webkit?.messageHandlers?.close?.postMessage({}); event.preventDefault(); return; }
    if (event.key === '/') { openSearch(); event.preventDefault(); return; }
    if (event.key === 'n' || event.key === 'N') { find(event.key === 'N'); event.preventDefault(); return; }
    if (event.key === 'r') { location.reload(); event.preventDefault(); return; }
    if (event.key === '[') { history.back(); event.preventDefault(); return; }
    if (event.key === ']') { history.forward(); event.preventDefault(); return; }
    if (event.key === '+' || event.key === '-' || event.key === '0') {
      zoom = event.key === '0' ? 1 : Math.max(0.5, Math.min(2, zoom + (event.key === '+' ? 0.1 : -0.1)));
      document.body.style.zoom = zoom;
      event.preventDefault();
      return;
    }
    if (event.key === 'Escape') { document.activeElement?.blur(); event.preventDefault(); }
  });
})();
