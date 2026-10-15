/**
 * /read [spec]: hand the transcript's turns to `limpid`, which renders them
 * as a styled HTML page and opens it. See turns.ts for how turns are cut.
 */
import type { Register } from 'claude-code'
import { collectTurns } from './turns'

type Opened = { file?: string; context?: string }

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
    const turns = collectTurns(Array.isArray(messages) ? messages : [])

    let model: string | undefined
    try {
      model = (await $.session.model()) || undefined
    } catch {
      model = undefined
    }

    const home = (await $.env.get('HOME')) ?? '~'
    const spec = (e.args ?? '').trim() || '0'
    const init = { stdin: JSON.stringify({ model, turns }), timeoutMs: TIMEOUT_MS }

    // `limpid` on PATH first, then where install.sh links it by default.
    let ran
    try {
      ran = await $.process.run(['limpid', '--spec', spec], init)
    } catch {
      try {
        ran = await $.process.run([`${home}/.local/bin/limpid`, '--spec', spec], init)
      } catch {
        $.ui.toast('limpid was not found. Install it from https://github.com/tjmisko/limpid')
        return {}
      }
    }

    if (ran.exitCode !== 0) {
      const reason = ran.stderr.trim().split(/\r?\n/)[0] || `limpid exited with ${ran.exitCode}`
      $.ui.toast(reason)
      return {}
    }

    let opened: Opened = {}
    try {
      opened = JSON.parse(ran.stdout) as Opened
    } catch {
      $.ui.toast('limpid printed output /read could not parse')
      return {}
    }
    $.ui.toast(`Opened ${opened.context ?? 'response'}.`)
    return {}
  })
}
