/** /read [spec]: pass transcript messages to the Rust reader, then open its page. */
import type { Register } from 'claude-code'

const ARGUMENT_HINT = '[0 | 0..2 | 1..=3 | 2.. | ..4]'
const TIMEOUT_MS = 30_000

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'read',
      description: 'Open a response, or a Rust-style range of turns, as a styled HTML page',
      argumentHint: ARGUMENT_HINT,
    })
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
    const init = { stdin: JSON.stringify({ model, session_id: sessionId, messages }), timeoutMs: TIMEOUT_MS }

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
    try { opened = await $.process.run(['limpid', path], { timeoutMs: TIMEOUT_MS }) }
    catch {
      try { opened = await $.process.run([`${home}/.local/bin/limpid`, path], { timeoutMs: TIMEOUT_MS }) }
      catch { return { text: 'limpid was not found' } }
    }
    if (opened.exitCode !== 0) return { text: opened.stderr.trim().split(/\r?\n/)[0] || `limpid exited with ${opened.exitCode}` }
    const note = `The user opened limpid page ${path}. Answers and comments arrive complete; only run \`limpid-core feedback ${path}\` if something is unclear.`
    return { context: [note] }
  })
}
