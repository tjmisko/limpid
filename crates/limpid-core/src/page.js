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
  const deletedComments = new Set();
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
    initFocus(true);
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
      note.dataset.commentKey = key;
      placeAtSectionEnd(note, target);
      currentComments.set(key, {key, src: range, turn, lineStart: comment.start_line || 0, lineEnd: comment.end_line || 0, quote: comment.quote || '', text: comment.text});
    }
    initFocus(true);
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
  const STOP_SELECTOR = 'article p[data-src],article blockquote[data-src],article figure.code[data-src],article figure.diagram[data-src],article div.table-wrap[data-src],article li[data-src],article .limpid-annotation';
  let focused = null;
  // Only the outermost stop counts, except that every list item is its own
  // stop. Question options are part of their field, not stops.
  function focusStops() {
    return [...document.querySelectorAll(STOP_SELECTOR)].filter(element => {
      if (element.matches('.question-option')) return false;
      if (element.matches('.limpid-annotation')) {
        return element.querySelector('strong')?.textContent.trim() === 'Comment';
      }
      if (element.matches('li')) return true;
      return !element.parentElement?.closest('li[data-src],blockquote[data-src]');
    });
  }
  function setFocus(element, reveal = false) {
    if (!element) return;
    if (focused !== element) {
      focused?.classList.remove('limpid-focused');
      element.classList.add('limpid-focused');
      focused = element;
    }
    if (reveal) revealStop(element);
  }
  function revealStop(element) {
    const rect = element.getBoundingClientRect();
    const margin = 12;
    if (rect.top < margin) window.scrollBy({top: rect.top - margin, behavior: 'instant'});
    else if (rect.bottom > innerHeight - margin) window.scrollBy({top: rect.bottom - (innerHeight - margin), behavior: 'instant'});
  }
  function moveFocus(delta) {
    const stops = focusStops();
    if (!stops.length) return;
    const index = stops.indexOf(focused);
    const next = index < 0
      ? (delta > 0 ? 0 : stops.length - 1)
      : Math.min(stops.length - 1, Math.max(0, index + delta));
    setFocus(stops[next], true);
  }
  function alignFocus(where) {
    if (!focused) return;
    const rect = focused.getBoundingClientRect();
    const top = where === 't' ? rect.top - 8
      : where === 'b' ? rect.bottom - innerHeight + 8
      : rect.top - (innerHeight - rect.height) / 2;
    window.scrollBy({top, behavior: 'instant'});
  }
  function sectionJump(direction) {
    const stops = focusStops();
    if (!stops.length) return;
    const position = focused
      ? focused.getBoundingClientRect().top + window.scrollY
      : window.scrollY + 25;
    const tops = headings.map(head => head.getBoundingClientRect().top + window.scrollY);
    const target = direction > 0
      ? tops.find(top => top > position + 2)
      : tops.filter(top => top < position - 2).at(-1);
    if (target === undefined) {
      setFocus(direction > 0 ? stops.at(-1) : stops[0], true);
      return;
    }
    const stop = stops.find(element => element.getBoundingClientRect().top + window.scrollY >= target - 1);
    setFocus(stop || (direction > 0 ? stops.at(-1) : stops[0]), true);
  }
  function initFocus(force = false) {
    if (!force && focused?.isConnected) return;
    const stops = focusStops();
    if (!stops.length) return;
    setFocus(stops.find(element => element.getBoundingClientRect().bottom > 0) || stops[0]);
  }
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
    const key = record.key || (record.turn ? `${record.turn}:${record.src}` : record.src);
    record.key = key;
    deletedComments.delete(key);
    currentComments.set(key, record);
    dirtyComments.add(key);
    return key;
  }
  function currentPayload() {
    return {replies: Object.fromEntries(currentReplies), comments: [...currentComments.values()]};
  }
  function dirtyPayload() {
    return {
      replies: Object.fromEntries([...dirtyReplies].map(field => [field, currentReplies.get(field)])),
      comments: [...dirtyComments].map(key => currentComments.get(key)),
      deletions: [...deletedComments],
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
    deletedComments.clear();
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
  // File pages render them inline, so give each one a key for edit and delete.
  for (const note of [...document.querySelectorAll('article .limpid-annotation')]) {
    if (note.querySelector('strong')?.textContent.trim() !== 'Comment') continue;
    if (!note.dataset.commentKey) {
      const range = note.dataset.src || '';
      if (range) {
        const key = `file:${range}`;
        note.dataset.commentKey = key;
        const body = note.querySelector('p')?.textContent || '';
        currentComments.set(key, {
          key,
          src: range,
          turn: '',
          lineStart: Number(note.dataset.lineStart || 0),
          lineEnd: Number(note.dataset.lineEnd || 0),
          quote: body.trim().slice(0, 80),
          text: body,
        });
      }
    }
    placeAtSectionEnd(note, note);
  }
  function commentRecord(target, quote) {
    return {
      src: target.dataset.src || '',
      turn: target.closest('.turn')?.id || '',
      lineStart: Number(target.dataset.lineStart || 0),
      lineEnd: Number(target.dataset.lineEnd || 0),
      quote,
    };
  }
  function saveComment(target, existingKey, quote, text) {
    const existing = existingKey ? currentComments.get(existingKey) : null;
    const record = existing ? {...existing} : commentRecord(target, quote);
    record.text = text;
    record.key = existingKey || '';
    const key = rememberComment(record);
    let note = existingKey
      ? document.querySelector(`.limpid-annotation[data-comment-key="${CSS.escape(existingKey)}"]`)
      : null;
    if (!note) {
      note = document.createElement('aside');
      note.className = 'limpid-annotation';
      const heading = document.createElement('strong');
      heading.textContent = 'Comment';
      const body = document.createElement('p');
      note.append(heading, body);
      placeAtSectionEnd(note, target);
    }
    note.dataset.commentKey = key;
    note.querySelector('p').textContent = text;
  }
  function deleteComment(note) {
    const key = note?.dataset.commentKey;
    if (!key) return;
    deletedComments.add(key);
    dirtyComments.delete(key);
    currentComments.delete(key);
    note.remove();
  }
  async function edit(target, kind) {
    if (editing || !target || !await loadEditor()) return;
    editing = true;
    let quote = '';
    let current = '';
    let commentKey = null;
    if (kind === 'reply') {
      if (!target.matches?.('.question-field')) { editing = false; return; }
      current = target.querySelector('.question-reply')?.textContent || '';
    } else if (target.classList.contains('limpid-annotation')) {
      commentKey = target.dataset.commentKey || null;
      const record = commentKey ? currentComments.get(commentKey) : null;
      current = record?.text || target.querySelector('p')?.textContent || '';
      quote = record?.quote || target.textContent.trim().slice(0, 80);
    } else {
      quote = target.textContent.trim().slice(0, 80);
    }
    const mount = document.createElement('div');
    mount.className = 'limpid-editor';
    placeAtSectionEnd(mount, target);
    const finish = () => {
      mount.remove();
      editing = false;
      if (target.isConnected) setFocus(target);
    };
    window.limpidCreateEditor(mount, current, text => {
      if (kind === 'reply') {
        target.querySelector('.question-reply').textContent = text;
        rememberReply(target.dataset.field, text);
      } else {
        saveComment(target, commentKey, quote, text);
      }
      finish();
    }, finish);
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
  let pendingZ = false;
  function clearPending() {
    pendingG = false;
    pendingZ = false;
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
      sectionJump(event.key === 'ArrowDown' ? 1 : -1);
      return;
    }
    if (event.ctrlKey && !event.altKey && !event.metaKey && (event.key === 'd' || event.key === 'u')) {
      event.preventDefault();
      window.scrollBy({top: (event.key === 'd' ? 1 : -1) * innerHeight / 2, behavior: 'instant'});
      return;
    }
    if (event.ctrlKey || event.altKey || event.metaKey) return;
    if (event.key === 'g') {
      event.preventDefault();
      if (pendingG) {
        clearPending();
        setFocus(focusStops()[0], true);
      } else {
        clearPending();
        pendingG = true;
        setTimeout(() => { pendingG = false; }, 650);
      }
      return;
    }
    if (event.key === 'z') {
      event.preventDefault();
      if (pendingZ) {
        clearPending();
        alignFocus('z');
      } else {
        clearPending();
        pendingZ = true;
        setTimeout(() => { pendingZ = false; }, 650);
      }
      return;
    }
    if (pendingZ && (event.key === 't' || event.key === 'b')) {
      event.preventDefault();
      const where = event.key;
      clearPending();
      alignFocus(where);
      return;
    }
    if (pendingG) clearPending();
    if (event.key === 'j' || event.key === 'k' || event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      moveFocus(event.key === 'j' || event.key === 'ArrowDown' ? 1 : -1);
      return;
    }
    const field = focused?.matches?.('.question-field') ? focused : null;
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
      edit(focused, 'comment');
      return;
    }
    if (event.key === 'x') {
      event.preventDefault();
      if (focused?.classList.contains('limpid-annotation')) deleteComment(focused);
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
    if (event.key === '{' || event.key === '}') {
      event.preventDefault();
      sectionJump(event.key === '}' ? 1 : -1);
      return;
    }
    if (event.key === 'PageDown' || event.key === 'PageUp') {
      event.preventDefault();
      window.scrollBy({top: (event.key === 'PageDown' ? 1 : -1) * innerHeight / 2, behavior: 'instant'});
      return;
    }
    if (event.key === 'G') {
      event.preventDefault();
      setFocus(focusStops().at(-1), true);
      return;
    }
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
  requestAnimationFrame(() => initFocus());
})();
