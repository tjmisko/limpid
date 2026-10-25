/** /read [spec]: pass transcript messages to the Rust reader, then open its page. */
import type { Register } from 'claude-code'

const ARGUMENT_HINT = '[0 | 0..2 | 1..=3 | 2.. | ..4]'
const TIMEOUT_MS = 30_000

// Binds the wake socket, then prints each frame before answering {ok}, so a
// listener whose session is gone fails the write and the host falls back.
const LISTENER = `
import os, socket, sys
path = sys.argv[1]
os.makedirs(os.path.dirname(path), mode=0o700, exist_ok=True)
try: os.unlink(path)
except FileNotFoundError: pass
server = socket.socket(socket.AF_UNIX)
server.bind(path)
os.chmod(path, 0o600)
server.listen()
while True:
    connection, _ = server.accept()
    with connection:
        connection.settimeout(5)
        buffer = b""
        try:
            while b"\\n" not in buffer:
                chunk = connection.recv(65536)
                if not chunk: break
                buffer += chunk
        except OSError: continue
        if b"\\n" not in buffer: continue
        sys.stdout.write(buffer.split(b"\\n", 1)[0].decode() + "\\n")
        sys.stdout.flush()
        connection.sendall(b'{"ok":true}\\n')
`

let feedbackSock: string | undefined
let listener: AsyncGenerator<unknown, unknown> | undefined

function stopListener(): void {
  void listener?.return(undefined)
  listener = undefined
  feedbackSock = undefined
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'read',
      description: 'Open a response, or a Rust-style range of turns, as a styled HTML page',
      argumentHint: ARGUMENT_HINT,
    })
    stopListener()
    let sessionId: string | undefined
    try { sessionId = await $.session.id() } catch { sessionId = undefined }
    if (!sessionId) return next(e)
    const runtime = await $.env.get('XDG_RUNTIME_DIR')
    const home = (await $.env.get('HOME')) ?? '~'
    const directory = runtime ? `${runtime}/limpid` : `${home}/.cache/limpid/run`
    const socket = `${directory}/claude-${sessionId}.sock`
    const stream = $.process.spawn({ argv: ['python3', '-c', LISTENER, socket] })
    listener = stream
    feedbackSock = socket
    void (async () => {
      let buffer = ''
      try {
        for await (const chunk of stream) {
          if (chunk.stream !== 'stdout') continue
          buffer += chunk.text
          let newline
          while ((newline = buffer.indexOf('\n')) >= 0) {
            const line = buffer.slice(0, newline)
            buffer = buffer.slice(newline + 1)
            try {
              const signal = JSON.parse(line)
              if (typeof signal.message === 'string') void $.prompt.submit({ text: signal.message })
            } catch { /* ignore malformed frames */ }
          }
        }
      } catch { /* listener could not start */ }
      if (listener === stream) {
        listener = undefined
        feedbackSock = undefined
      }
    })()
    return next(e)
  })

  on('session.end', async ($, e, next) => {
    stopListener()
    return next(e)
  })

  on('command.run', { command: 'read' }, async ($, e) => {
    const messages = await $.session.messages()
    let model: string | undefined
    try { model = (await $.session.model()) || undefined } catch { model = undefined }
    let sessionId: string | undefined
    try { sessionId = await $.session.id() } catch { sessionId = undefined }
    const home = (await $.env.get('HOME')) ?? '~'
    const spec = (e.args ?? '').trim() || '0'
    const init = { stdin: JSON.stringify({ model, session_id: sessionId, messages, feedback_sock: feedbackSock }), timeoutMs: TIMEOUT_MS }

    let rendered
    try {
      rendered = await $.process.run(['limpid-core', 'claude', spec], init)
    } catch {
      try { rendered = await $.process.run([`${home}/.local/bin/limpid-core`, 'claude', spec], init) }
      catch { return { text: 'limpid-core was not found' } }
    }
    if (rendered.exitCode !== 0) {
      return { text: rendered.stderr.trim().split(/\r?\n/)[0] || `limpid-core exited with ${rendered.exitCode}` }
    }
    const path = rendered.stdout.trim()
    if (!path) return { text: 'limpid-core did not return a page path' }

    let opened
    try { opened = await $.process.run(['limpid', '--source-backed', path], { timeoutMs: TIMEOUT_MS }) }
    catch {
      try { opened = await $.process.run([`${home}/.local/bin/limpid`, '--source-backed', path], { timeoutMs: TIMEOUT_MS }) }
      catch { return { text: 'limpid was not found' } }
    }
    if (opened.exitCode !== 0) return { text: opened.stderr.trim().split(/\r?\n/)[0] || `limpid exited with ${opened.exitCode}` }
    const note = `The user opened limpid page ${path}. Answers and comments arrive complete; only run \`limpid-core feedback ${path}\` if something is unclear.`
    return { context: [note] }
  })
}
