import type { On } from 'claude-code'
import { expect, test } from 'claude-code/testing'
import { collectTurns } from './turns'

const RUN = { origin: { kind: 'composer' as const }, presentation: { isFullscreen: false, columns: 120 } }

const user = (text: string) => ({ role: 'user' as const, text, toolUses: [] })
const assistant = (text: string) => ({ role: 'assistant' as const, text, toolUses: [] })
const toolResultOnly = () => ({
  role: 'user' as const,
  text: '',
  toolUses: [],
  toolResults: [{ tool_use_id: 't1', text: 'output', isError: false, result: 'output' }],
})
const readInvocation = (args = '') =>
  user(`<command-name>/read</command-name>\n<command-message>read</command-message>\n<command-args>${args}</command-args>`)

// --- collectTurns --------------------------------------------------------

test('should return no turns when the transcript is empty', () => {
  expect(collectTurns([])).toEqual([])
})

test('should pair the prompt with its response when there is one exchange', () => {
  expect(collectTurns([user('hi'), assistant('hello')])).toEqual([{ prompt: 'hi', response: 'hello' }])
})

test('should return turns oldest first when there are several exchanges', () => {
  const turns = collectTurns([user('a'), assistant('A'), user('b'), assistant('B')])
  expect(turns.map(t => t.prompt)).toEqual(['a', 'b'])
})

test('should keep the last assistant text when tool calls separate several assistant messages', () => {
  const turns = collectTurns([
    user('do it'),
    assistant('Let me look.'),
    toolResultOnly(),
    assistant('Still looking.'),
    toolResultOnly(),
    assistant('Done: the answer.'),
  ])
  expect(turns).toEqual([{ prompt: 'do it', response: 'Done: the answer.' }])
})

test('should keep earlier text when the final assistant message has no text', () => {
  const turns = collectTurns([user('go'), assistant('Working.'), toolResultOnly(), assistant('')])
  expect(turns).toEqual([{ prompt: 'go', response: 'Working.' }])
})

test('should not split a turn when a user message carries only tool results', () => {
  const turns = collectTurns([user('q'), assistant('a1'), toolResultOnly(), assistant('a2')])
  expect(turns.length).toBe(1)
})

test('should not start a turn when the user message holds only a system-reminder', () => {
  const turns = collectTurns([
    user('q'),
    assistant('a'),
    user('<system-reminder>\nharness\nnotes\n</system-reminder>'),
    assistant('late'),
  ])
  expect(turns).toEqual([{ prompt: 'q', response: 'late' }])
})

test('should strip system-reminder blocks when the prompt is real', () => {
  const turns = collectTurns([
    user('<system-reminder>\nmulti\nline\n</system-reminder>\nreal question\n<system-reminder>x</system-reminder>'),
    assistant('answer'),
  ])
  expect(turns).toEqual([{ prompt: 'real question', response: 'answer' }])
})

test('should drop the turn when the assistant never produced text', () => {
  expect(collectTurns([user('q'), assistant('')])).toEqual([])
  expect(collectTurns([user('q'), assistant('   \n')])).toEqual([])
  expect(collectTurns([user('q')])).toEqual([])
})

test('should ignore assistant text when no user message precedes it', () => {
  expect(collectTurns([assistant('orphan'), user('q'), assistant('a')])).toEqual([{ prompt: 'q', response: 'a' }])
})

test('should drop the trailing tagged /read invocation when it is the last user message', () => {
  const turns = collectTurns([user('q'), assistant('a'), readInvocation('0..2')])
  expect(turns).toEqual([{ prompt: 'q', response: 'a' }])
})

test('should drop the trailing /read invocation when it is recorded as bare text', () => {
  expect(collectTurns([user('q'), assistant('a'), user('/read 1')])).toEqual([{ prompt: 'q', response: 'a' }])
  expect(collectTurns([user('q'), assistant('a'), user('/read')])).toEqual([{ prompt: 'q', response: 'a' }])
})

test('should drop the trailing /read invocation when local-command output follows it', () => {
  const turns = collectTurns([
    user('q'),
    assistant('a'),
    readInvocation(),
    user('<local-command-stdout></local-command-stdout>'),
  ])
  expect(turns).toEqual([{ prompt: 'q', response: 'a' }])
})

test('should return every turn when the /read invocation is absent', () => {
  expect(collectTurns([user('q'), assistant('a')]).length).toBe(1)
})

test('should keep an earlier /read as a turn when it has a response', () => {
  const turns = collectTurns([user('q'), assistant('a'), readInvocation('0'), assistant('opened'), user('next'), assistant('b'), readInvocation('1')])
  expect(turns.map(t => t.response)).toEqual(['a', 'opened', 'b'])
})

test('should drop an earlier /read when it has no response', () => {
  const turns = collectTurns([user('q'), assistant('a'), readInvocation('0'), user('next'), assistant('b'), readInvocation('0')])
  expect(turns.map(t => t.prompt)).toEqual(['q', 'next'])
})

test('should not drop a prompt that merely mentions /read when it is not an invocation', () => {
  const turns = collectTurns([user('what does /read do?'), assistant('it opens pages')])
  expect(turns.length).toBe(1)
})

test('should not drop other trailing slash commands when they are not /read', () => {
  const turns = collectTurns([user('<command-name>/readme</command-name>'), assistant('x')])
  expect(turns.length).toBe(1)
})

test('should preserve unicode and very long text when collecting', () => {
  const long = 'x'.repeat(200_000)
  const turns = collectTurns([user('日本語 prompt 🎉 ünïcode'), assistant(`${long}\nλ → ∑`)])
  expect(turns[0]?.prompt).toBe('日本語 prompt 🎉 ünïcode')
  expect(turns[0]?.response).toBe(`${long}\nλ → ∑`)
})

// --- /read command wiring -----------------------------------------------------

type Call = { argv: readonly string[]; stdin?: string; timeoutMs?: number }

function wire(on: On, run: (call: Call) => unknown, extra: { model?: string } = {}) {
  const calls: Call[] = []
  const toasts: string[] = []
  on('session.messages', () => ({ value: [user('q'), assistant('a'), readInvocation('0..2')] }))
  on('session.model', () => ({ value: extra.model ?? 'opus' }))
  on('env.get', () => ({ value: '/home/test' }))
  on('ui.toast', (_$, e) => {
    toasts.push(e.text)
    return { value: undefined }
  })
  on('process.run', (_$, e) => {
    const call = { argv: e.argv, stdin: e.init?.stdin, timeoutMs: e.init?.timeoutMs }
    calls.push(call)
    return run(call) as never
  })
  return { calls, toasts }
}

const ok = (stdout: string) => ({ value: { exitCode: 0, stdout, stderr: '', isStdoutTruncated: false, isStderrTruncated: false } })

test('should pass the spec and turns to limpid when /read runs', async ($, on) => {
  const { calls } = wire(on, () => ok('{"file":"/x.html","context":"turn 0"}'))
  await $.command.run({ ...RUN, command: 'read', args: ' 0..2 ' })
  expect(calls.length).toBe(1)
  expect(calls[0]?.argv).toEqual(['limpid', '--spec', '0..2'])
  expect(JSON.parse(calls[0]?.stdin ?? '{}')).toEqual({ model: 'opus', turns: [{ prompt: 'q', response: 'a' }] })
  expect(calls[0]?.timeoutMs).toBe(30000)
})

test('should default the spec to 0 when /read has no arguments', async ($, on) => {
  const { calls } = wire(on, () => ok('{"file":"/x.html","context":"turn 0"}'))
  await $.command.run({ ...RUN, command: 'read', args: '' })
  expect(calls[0]?.argv.at(-1)).toBe('0')
})

test('should toast the opened context when limpid succeeds', async ($, on) => {
  const { toasts } = wire(on, () => ok('{"file":"/x.html","context":"turns 0–2"}'))
  const result = await $.command.run({ ...RUN, command: 'read', args: '0..3' })
  expect(toasts).toEqual(['Opened turns 0–2.'])
  expect(result.text).toBe(undefined)
})

test('should toast the first stderr line when limpid exits non-zero', async ($, on) => {
  const { toasts } = wire(on, () => ({
    value: { exitCode: 1, stdout: '', stderr: '  Range covers 11 turns; the maximum is 10.\nusage: ...\n', isStdoutTruncated: false, isStderrTruncated: false },
  }))
  await $.command.run({ ...RUN, command: 'read', args: '0..11' })
  expect(toasts).toEqual(['Range covers 11 turns; the maximum is 10.'])
})

test('should toast a generic message when limpid fails silently', async ($, on) => {
  const { toasts } = wire(on, () => ({
    value: { exitCode: 2, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false },
  }))
  await $.command.run({ ...RUN, command: 'read', args: '' })
  expect(toasts).toEqual(['limpid exited with 2'])
})

test('should fall back to ~/.local/bin/limpid when limpid is not on PATH', async ($, on) => {
  const { calls, toasts } = wire(on, call =>
    call.argv[0] === 'limpid' ? { deny: 'ENOENT' } : ok('{"file":"/x.html","context":"turn 0"}'),
  )
  await $.command.run({ ...RUN, command: 'read', args: '1' })
  expect(calls.map(c => c.argv[0])).toEqual(['limpid', '/home/test/.local/bin/limpid'])
  expect(toasts).toEqual(['Opened turn 0.'])
})

test('should toast an install hint when limpid cannot start from either location', async ($, on) => {
  const { toasts } = wire(on, () => ({ deny: 'ENOENT' }))
  await $.command.run({ ...RUN, command: 'read', args: '' })
  expect(toasts.length).toBe(1)
  expect(toasts[0]?.includes('Install it')).toBe(true)
})

test('should toast a warning when limpid prints something other than JSON', async ($, on) => {
  const { toasts } = wire(on, () => ok('not json'))
  await $.command.run({ ...RUN, command: 'read', args: '' })
  expect(toasts.length).toBe(1)
  expect(toasts[0]?.includes('parse')).toBe(true)
})
