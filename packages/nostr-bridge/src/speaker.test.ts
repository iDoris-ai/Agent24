import { execFile } from 'node:child_process'
import { describe, expect, it, vi } from 'vitest'
import {
  groupCliRunner,
  SpeakerClient,
  type GroupHistoryEntry,
  type InboundMessage,
} from './speaker.js'

vi.mock('node:child_process', () => ({
  execFile: vi.fn(),
}))

const groupPlain = 'fixture group hello'
const dmPlain = 'fixture dm hello'
const groupId = '00112233445566778899aabbccddeeff'
const logicalId = 'ffeeddccbbaa99887766554433221100'

function envelope(data: unknown): string {
  return JSON.stringify({ ok: true, data })
}

function groupRow(): Record<string, unknown> {
  return {
    id: logicalId,
    event_id: 'ab'.repeat(32),
    sender: 'npub1alice',
    plaintext: groupPlain,
    created_at: 1_700_000_000,
  }
}

describe('group history', () => {
  it('returns the fixture group row and leaves it out of history inbox', async () => {
    const calls: string[][] = []
    const client = new SpeakerClient(
      async (args) => {
        calls.push(args)
        if (args[0] === 'groupchat' && args[1] === 'history') {
          return envelope({ messages: [groupRow()] })
        }
        if (args[0] === 'history' && args[1] === 'inbox') {
          return envelope([
            {
              id: 'cd'.repeat(32),
              sender_npub: 'npub1bob',
              plaintext: dmPlain,
              is_incoming: true,
              created_at: 1_700_000_001,
            },
          ])
        }
        throw new Error(`unexpected argv ${args.join(' ')}`)
      },
      { identity: 'alice' },
    )

    const groups = await client.groupHistory(groupId)
    const inbox: InboundMessage[] = await client.inbox()

    expect(groups).toEqual<GroupHistoryEntry[]>([
      {
        id: logicalId,
        sender: 'npub1alice',
        plaintext: groupPlain,
        event_id: 'ab'.repeat(32),
        created_at: 1_700_000_000,
      },
    ])
    expect(inbox.map((row) => row.content)).toEqual([dmPlain])
    expect(inbox.map((row) => row.content)).not.toContain(groupPlain)
    expect(calls[0]).toEqual([
      'groupchat',
      'history',
      '--group-id',
      groupId,
      '--identity',
      'alice',
      '--limit',
      '20',
      '--json',
    ])
    expect(calls[1]?.slice(0, 2)).toEqual(['history', 'inbox'])
  })

  it('rejects a call with no identity', async () => {
    const client = new SpeakerClient(async () => envelope({ messages: [] }))
    await expect(client.groupHistory(groupId)).rejects.toThrow(/requires an identity/)
  })

  it('rejects an empty group id', async () => {
    const client = new SpeakerClient(async () => envelope({ messages: [] }), { identity: 'alice' })
    await expect(client.groupHistory('')).rejects.toThrow(/requires a group id/)
  })

  it('fails the call when hyphae returns a non-zero envelope', async () => {
    const client = new SpeakerClient(
      async () => JSON.stringify({ ok: false, message: 'group is not active' }),
      {
        identity: 'alice',
      },
    )
    await expect(client.groupHistory(groupId)).rejects.toThrow(/group is not active/)
  })

  it('rejects a bare binary name before execFile', () => {
    expect(() => groupCliRunner('hyphae')).toThrow(/not pinned/)
    expect(vi.mocked(execFile)).not.toHaveBeenCalled()
  })

  it('passes the absolute binary path to execFile', async () => {
    vi.mocked(execFile).mockImplementation(((
      _bin: string,
      _args: readonly string[],
      _opts: object,
      cb: (err: Error | null, stdout: string, stderr: string) => void,
    ) => {
      cb(null, envelope({ messages: [] }), '')
    }) as unknown as typeof execFile)
    const client = new SpeakerClient(groupCliRunner('/opt/hyphae/hyphae'), { identity: 'alice' })
    await expect(client.groupHistory(groupId)).resolves.toEqual([])
    expect(vi.mocked(execFile)).toHaveBeenCalledWith(
      '/opt/hyphae/hyphae',
      [
        'groupchat',
        'history',
        '--group-id',
        groupId,
        '--identity',
        'alice',
        '--limit',
        '20',
        '--json',
      ],
      expect.objectContaining({ killSignal: 'SIGKILL' }),
      expect.any(Function),
    )
  })
})

describe('group send', () => {
  it('rejects a call with no identity', async () => {
    const client = new SpeakerClient(async () => envelope({}))
    await expect(client.groupSend(groupId, 'hi')).rejects.toThrow(/requires an identity/)
  })

  it('rejects an empty group id', async () => {
    const client = new SpeakerClient(async () => envelope({}), { identity: 'alice' })
    await expect(client.groupSend('', 'hi')).rejects.toThrow(/requires a group id/)
  })

  it('rejects an empty body', async () => {
    const client = new SpeakerClient(async () => envelope({}), { identity: 'alice' })
    await expect(client.groupSend(groupId, '')).rejects.toThrow(/requires a body/)
  })

  it('fails the call when hyphae returns a non-zero envelope', async () => {
    const client = new SpeakerClient(
      async () => JSON.stringify({ ok: false, message: 'group is not active' }),
      { identity: 'alice' },
    )
    await expect(client.groupSend(groupId, 'hi')).rejects.toThrow(/group is not active/)
  })
})
