import { expect, test } from 'claude-code/testing'

// Stands in for the engine: a session id that /clear changes, a listener that
// never exits, and limpid-core / limpid runs that record what they were sent.
function engine(on: Parameters<Parameters<typeof test>[1]>[1], state: { id: string, sent: string[], spawned: string[] }) {
  on('command.register', () => ({ value: { command: 'read' } }))
  on('session.start', (_, e) => ({ cwd: e.cwd }))
  on('session.end', (_, e) => ({ sessionId: e.sessionId }))
  on('session.id', () => ({ value: state.id }))
  on('session.messages', () => ({ value: [] }))
  on('session.model', () => ({ value: 'test-model' }))
  on('env.get', (_, e) => ({ value: e.name === 'XDG_RUNTIME_DIR' ? '/run/test' : '/home/test' }))
  on('process.spawn', async function* (_, e) {
    state.spawned.push(e.argv.at(-1) ?? '')
    await new Promise(() => {})
    return { code: 0, signal: null }
  })
  on('process.run', (_, e) => {
    if (e.argv.at(-2) === 'claude') {
      state.sent.push(e.init?.stdin ?? '')
      return { value: { exitCode: 0, stdout: '/tmp/page.html\n', stderr: '' } }
    }
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
}

function feedbackSock(stdin: string | undefined): unknown {
  return JSON.parse(stdin ?? '{}').feedback_sock
}

test('should pass the session socket to limpid-core when /read runs after startup', async ($, on) => {
  const state = { id: 'first', sent: [] as string[], spawned: [] as string[] }
  engine(on, state)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await $.command.run({ command: 'read', args: '' } as never)
  expect(feedbackSock(state.sent[0])).toBe('/run/test/limpid/claude-first.sock')
  expect(state.spawned).toEqual(['/run/test/limpid/claude-first.sock'])
})

test('should bind a socket for the new session when /read runs after /clear', async ($, on) => {
  const state = { id: 'first', sent: [] as string[], spawned: [] as string[] }
  engine(on, state)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await $.session.end({ reason: 'clear', sessionId: 'first', resume: { sessionId: 'first' } } as never)
  state.id = 'second'
  await $.command.run({ command: 'read', args: '' } as never)
  expect(feedbackSock(state.sent[0])).toBe('/run/test/limpid/claude-second.sock')
  expect(state.spawned).toEqual(['/run/test/limpid/claude-first.sock', '/run/test/limpid/claude-second.sock'])
})

test('should reuse the listener when /read runs twice in one session', async ($, on) => {
  const state = { id: 'first', sent: [] as string[], spawned: [] as string[] }
  engine(on, state)
  await $.session.start({ cwd: '/', surface: 'terminal', isInteractive: true })
  await $.command.run({ command: 'read', args: '' } as never)
  await $.command.run({ command: 'read', args: '1' } as never)
  expect(state.spawned).toEqual(['/run/test/limpid/claude-first.sock'])
  expect(feedbackSock(state.sent[1])).toBe('/run/test/limpid/claude-first.sock')
})
