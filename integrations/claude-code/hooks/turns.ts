/**
 * Pure helpers for /read: grouping the transcript into prompt -> response
 * turns (oldest first) and dropping the /read invocation that is running.
 */

export type MessageLike = {
  role: 'user' | 'assistant'
  text: string
  toolResults?: readonly unknown[]
}

export type Turn = { prompt: string; response: string }

const SYSTEM_REMINDER = /<system-reminder>[\s\S]*?<\/system-reminder>/g

/** Output the harness echoes back as user-role rows; never a prompt. */
const LOCAL_COMMAND_OUTPUT = /<local-command-(?:stdout|stderr|caveat)>[\s\S]*?<\/local-command-(?:stdout|stderr|caveat)>/g

/** How a typed `/read args` is recorded: tagged, or as the bare text. */
const READ_INVOCATION = /^(?:<command-name>\s*\/?read\s*<\/command-name>|\/read(?:\s|$))/

/** A user message's text with system-reminder blocks removed, trimmed. */
export function promptTextOf(text: string): string {
  return text.replace(SYSTEM_REMINDER, '').trim()
}

function startsTurn(message: MessageLike): boolean {
  if (message.role !== 'user') return false
  return promptTextOf(message.text).replace(LOCAL_COMMAND_OUTPUT, '').trim() !== ''
}

export function isReadInvocation(prompt: string): boolean {
  return READ_INVOCATION.test(prompt.trim())
}

/**
 * Turns oldest first. A turn opens at a user message with real prompt text;
 * its response is the last assistant message in it that has text. Turns
 * without a response are dropped, as is a trailing /read invocation (the
 * command that is running now).
 */
export function collectTurns(messages: readonly MessageLike[]): Turn[] {
  type Open = { prompt: string; response: string }
  const opened: Open[] = []
  let current: Open | undefined

  for (const message of messages) {
    if (startsTurn(message)) {
      current = { prompt: promptTextOf(message.text), response: '' }
      opened.push(current)
      continue
    }
    if (message.role !== 'assistant' || !current) continue
    if (message.text.trim() !== '') current.response = message.text.trim()
  }

  const last = opened.at(-1)
  if (last && isReadInvocation(last.prompt)) opened.pop()

  return opened.filter(turn => turn.response !== '')
}
