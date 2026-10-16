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
  const fields = [...document.querySelectorAll('.question-field')];
  const currentReplies = new Map();
  const currentComments = new Map();
  const dirtyReplies = new Set();
  const dirtyComments = new Set();
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
      currentReplies.set(id, text);
      for (const option of field.querySelectorAll('.question-option')) {
        option.classList.toggle('selected', option.textContent.trim() === text.trim());
      }
    }
  };
  for (const field of fields) {
    const answer = field.querySelector('.question-reply')?.textContent?.trim();
    if (!answer) continue;
    currentReplies.set(field.dataset.field, answer);
    for (const option of field.querySelectorAll('.question-option')) {
      option.classList.toggle('selected', option.textContent.trim() === answer);
    }
  }
  window.limpidRestoreComments = comments => {
    for (const [key, comment] of Object.entries(comments)) {
      const [turn, range] = key.includes(':') ? key.split(':', 2) : ['', key];
      const target = [...document.querySelectorAll('article [data-src]')]
        .findLast(block => block.dataset.src === range && (block.closest('.turn')?.id || '') === turn);
      if (!target) continue;
      const note = document.createElement('aside');
      note.className = 'limpid-annotation';
      const heading = document.createElement('strong');
      heading.textContent = 'Comment';
      const text = document.createElement('p');
      text.textContent = comment.text;
      note.append(heading, text);
      placeAtSectionEnd(note, target);
      currentComments.set(key, {src: range, turn, lineStart: comment.start_line || 0, lineEnd: comment.end_line || 0, quote: comment.quote || '', text: comment.text});
    }
  };
  const headings = [...document.querySelectorAll('article h1[id],article h2[id],article h3[id],article h4[id],article h5[id],article h6[id]')];
  // Highlight the section at the current scroll position. The page top and
  // bottom always select the first and last section.
  function updateScrollSpy() {
    if (!links.length || !headings.length) return;
    const atTop = window.scrollY <= 1;
    const atBottom = window.scrollY + window.innerHeight >= document.documentElement.scrollHeight - 1;
    const threshold = 90;
    const head = atTop ? headings[0]
      : atBottom ? headings.at(-1)
      : headings.filter(item => item.getBoundingClientRect().top <= threshold).at(-1) || headings[0];
    let index = headings.indexOf(head);
    while (index > 0 && !byId.has(headings[index].id)) index -= 1;
    const active = byId.get(headings[index].id) || [];
    links.forEach(link => link.classList.toggle('active', active.includes(link)));
  }
  let spyQueued = false;
  const queueScrollSpy = () => {
    if (spyQueued) return;
    spyQueued = true;
    requestAnimationFrame(() => { spyQueued = false; updateScrollSpy(); });
  };
  window.addEventListener('scroll', queueScrollSpy, {passive: true});
  window.addEventListener('resize', queueScrollSpy);
  updateScrollSpy();
  let search = '';
  let searchBox;
  let commandBox;
  let zoom = 1;
  let pendingG = false;
  let editing = false;
  // The section that owns an element ends just before the next heading of the
  // same or shallower level, else at the end of the article.
  function sectionEndFor(element) {
    const article = element.closest('article') || document.querySelector('article') || document.body;
    const all = [...article.querySelectorAll('h1,h2,h3,h4,h5,h6')];
    const preceding = all.filter(head => head.compareDocumentPosition(element) & Node.DOCUMENT_POSITION_FOLLOWING);
    const heading = preceding.at(-1);
    const level = heading ? Number(heading.tagName.slice(1)) : 7;
    const following = all.filter(head => (head.compareDocumentPosition(element) & Node.DOCUMENT_POSITION_PRECEDING) && Number(head.tagName.slice(1)) <= level);
    return {article, before: following[0] || null};
  }
  function placeAtSectionEnd(node, element) {
    const {article, before} = sectionEndFor(element);
    article.insertBefore(node, before);
  }
  function post(name, payload) {
    window.webkit?.messageHandlers?.[name]?.postMessage(payload ?? {});
  }
  function rememberReply(field, text) {
    currentReplies.set(field, text);
    dirtyReplies.add(field);
  }
  function rememberComment(record) {
    const key = record.turn ? `${record.turn}:${record.src}` : record.src;
    currentComments.set(key, record);
    dirtyComments.add(key);
  }
  function currentPayload() {
    return {replies: Object.fromEntries(currentReplies), comments: [...currentComments.values()]};
  }
  function dirtyPayload() {
    return {
      replies: Object.fromEntries([...dirtyReplies].map(field => [field, currentReplies.get(field)])),
      comments: [...dirtyComments].map(key => currentComments.get(key)),
    };
  }
  function openCommand() {
    if (!commandBox) {
      commandBox = document.createElement('input');
      commandBox.className = 'limpid-command';
      commandBox.type = 'text';
      commandBox.setAttribute('aria-label', 'Command');
      commandBox.placeholder = ':q  :w  :wq  :q!  :qa';
      commandBox.addEventListener('keydown', event => {
        if (event.key === 'Escape') {
          closeCommand();
          event.preventDefault();
        } else if (event.key === 'Enter') {
          const command = commandBox.value.trim();
          closeCommand();
          runCommand(command);
          event.preventDefault();
        }
      });
      document.body.append(commandBox);
    }
    commandBox.hidden = false;
    commandBox.value = '';
    commandBox.focus();
  }
  function closeCommand() {
    if (!commandBox) return;
    commandBox.hidden = true;
    commandBox.blur();
  }
  function runCommand(command) {
    if (command === 'q') {
      if (dirtyReplies.size || dirtyComments.size) {
        window.limpidShowError?.('unsaved changes — :wq to save and send, :q! to discard');
      } else {
        post('close', {});
      }
    } else if (command === 'w') {
      post('save', dirtyPayload());
    } else if (command === 'wq') {
      post('deliver', dirtyPayload());
    } else if (command === 'q!') {
      post('close', {});
    } else if (command === 'qa') {
      post('close-all', {});
    } else if (command) {
      window.limpidShowError?.(`Not a command: :${command}`);
    }
  }
  window.limpidSaved = () => {
    dirtyReplies.clear();
    dirtyComments.clear();
  };
  for (const figure of document.querySelectorAll('figure.code,figure.diagram')) {
    const pre = figure.querySelector('pre');
    if (!pre) continue;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'limpid-copy';
    button.textContent = 'Copy';
    button.title = 'Copy code';
    button.addEventListener('click', () => {
      copyText(pre.textContent ?? '').then(ok => {
        button.textContent = ok ? 'Copied' : 'Copy failed';
        button.classList.toggle('copied', ok);
        setTimeout(() => { button.textContent = 'Copy'; button.classList.remove('copied'); }, 1400);
      });
    });
    figure.append(button);
  }
  async function copyText(text) {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
        return true;
      }
    } catch { /* Fall through to the selection copy. */ }
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('aria-hidden', 'true');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.append(area);
    area.select();
    let ok = false;
    try { ok = document.execCommand('copy'); } catch { /* Leave ok false. */ }
    area.remove();
    return ok;
  }
  // Saved comments are stored beside their block; show them at the section end.
  for (const note of [...document.querySelectorAll('article .limpid-annotation')]) {
    if (note.querySelector('strong')?.textContent.trim() !== 'Comment') continue;
    placeAtSectionEnd(note, note);
  }
  function jumpSection(direction) {
    const fromToc = [...document.querySelectorAll('nav a[href^="#"]')]
      .map(link => document.getElementById(decodeURIComponent(link.hash.slice(1))))
      .filter(Boolean);
    const targets = fromToc.length ? fromToc : [...document.querySelectorAll('h1, h2, h3')];
    const tops = [...new Set(targets
      .filter(head => head.getClientRects().length)
      .map(head => {
        const margin = parseFloat(getComputedStyle(head).scrollMarginTop) || 0;
        return Math.round(head.getBoundingClientRect().top + window.scrollY - margin);
      }))].sort((a, b) => a - b);
    const position = window.scrollY;
    const target = direction > 0
      ? tops.find(top => top > position + 8)
      : tops.reverse().find(top => top < position - 8);
    const top = target !== undefined ? target : direction < 0 ? 0 : position;
    window.scrollTo({top, behavior: 'instant'});
  }
  async function edit(target, kind) {
    if (editing || !target || !await loadEditor()) return;
    editing = true;
    const quote = target.textContent.trim().slice(0, 80);
    const mount = document.createElement('div');
    mount.className = 'limpid-editor';
    placeAtSectionEnd(mount, target);
    const current = kind === 'reply' ? target.querySelector('.question-reply')?.textContent || '' : '';
    const finish = () => { mount.remove(); editing = false; target.focus?.(); };
    window.limpidCreateEditor(mount, current, text => {
      if (kind === 'reply') {
        target.querySelector('.question-reply').textContent = text;
        rememberReply(target.dataset.field, text);
      } else {
        const note = document.createElement('aside');
        note.className = 'limpid-annotation';
        note.textContent = `Comment: ${text}`;
        placeAtSectionEnd(note, target);
        rememberComment({src: target.dataset.src, turn: target.closest('.turn')?.id || '', lineStart: Number(target.dataset.lineStart || 0), lineEnd: Number(target.dataset.lineEnd || 0), quote, text});
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
  window.limpidShowError = message => {
    let error = document.querySelector('.limpid-error');
    if (!error) {
      error = document.createElement('div');
      error.className = 'limpid-error';
      error.setAttribute('role', 'status');
      document.body.append(error);
    }
    error.textContent = message;
  };
  function centerMatch() {
    const selection = window.getSelection();
    if (!selection?.rangeCount) return;
    const rect = selection.getRangeAt(0).getBoundingClientRect();
    if (!rect || (!rect.width && !rect.height)) return;
    const target = rect.top + window.scrollY - innerHeight / 2 + rect.height / 2;
    window.scrollTo({top: Math.max(0, target), behavior: 'instant'});
  }
  function find(backwards = false) {
    if (!search) return false;
    const smartcase = /[A-Z]/.test(search);
    const found = window.find(search, smartcase, backwards, true);
    if (found) centerMatch();
    return found;
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
    if (event.key === 'F1') {
      event.preventDefault();
      document.querySelector('.shell')?.classList.toggle('rail-hidden');
      return;
    }
    if (editable()) return;
    if (event.ctrlKey && !event.altKey && !event.metaKey && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault();
      jumpSection(event.key === 'ArrowDown' ? 1 : -1);
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    if (event.key === 'g') {
      if (pendingG) {
        window.scrollTo({top: 0, behavior: 'instant'});
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
      rememberReply(field.dataset.field, answer);
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
      post('review', currentPayload());
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
      window.scrollBy({top: scroll, behavior: 'instant'});
      event.preventDefault();
      return;
    }
    if (event.key === 'G') { window.scrollTo({top: document.body.scrollHeight, behavior: 'instant'}); event.preventDefault(); return; }
    if (event.key === ':') { openCommand(); event.preventDefault(); return; }
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
