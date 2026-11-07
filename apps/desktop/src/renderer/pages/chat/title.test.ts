import { describe, expect, it } from 'vitest'
import { answeredTurns, cleanModelTitle, promptTitle, renameTitle, titleDue, titleRequest } from './title'

describe('conversation titles (UI-04)', () => {
  it('takes the first prompt on one line, cut to 60 characters', () => {
    expect(promptTitle('  周报\n\n 草稿  ')).toBe('周报 草稿')
    expect(promptTitle('字'.repeat(60))).toBe('字'.repeat(60))
    expect(promptTitle('字'.repeat(61))).toBe(`${'字'.repeat(60)}…`)
    expect(promptTitle('')).toBe('')
  })

  it('never splits an emoji or a letter with its accent', () => {
    const thumbs = '👍🏽' // four UTF-16 units, one character
    expect(promptTitle(`${'a'.repeat(59)}${thumbs}tail`)).toBe(`${'a'.repeat(59)}${thumbs}…`)
    expect(promptTitle(`${'a'.repeat(59)}éx`)).toBe(`${'a'.repeat(59)}é…`)
  })

  it('keeps a rename to 80 characters on one line, without an ellipsis', () => {
    expect(renameTitle(' 新\t名字 ')).toBe('新 名字')
    expect(renameTitle('字'.repeat(81))).toBe('字'.repeat(80))
    expect(renameTitle(`${'字'.repeat(79)} 尾`)).toBe(`${'字'.repeat(79)}`)
    expect(renameTitle(' \n ')).toBe('')
  })
})

describe('titles written by the model (UI-04 T2)', () => {
  it('cleans the answer: thinking, label, quotes, markdown and final punctuation go', () => {
    expect(cleanModelTitle('<think>想一想……</think>\n\n「周报草稿」。')).toBe('周报草稿')
    expect(cleanModelTitle('标题：**本周 周报**\n解释：……')).toBe('本周 周报')
    expect(cleanModelTitle('Title: "Weekly report draft".')).toBe('Weekly report draft')
    expect(cleanModelTitle('  \n# 请假流程？\n')).toBe('请假流程')
  })

  it('gives nothing when thinking never ended, or nothing is left', () => {
    expect(cleanModelTitle('<think>还在想，被截断了')).toBe('')
    expect(cleanModelTitle('“”。')).toBe('')
    expect(cleanModelTitle('')).toBe('')
  })

  it('cuts a long answer to 24 characters without splitting one', () => {
    expect(cleanModelTitle('字'.repeat(30))).toBe('字'.repeat(24))
    expect(cleanModelTitle(`${'字'.repeat(23)}👍🏽尾`)).toBe(`${'字'.repeat(23)}👍🏽`)
  })

  const user = (content: string, kind = 'sent') => ({ role: 'user' as const, content, delivery: { kind } })
  const reply = (content: string) => ({ role: 'assistant' as const, content })

  it('counts only turns that reached the model and were answered', () => {
    expect(answeredTurns([user('一', 'notSent'), reply('出错了'), user('二'), reply('答二'), user('三', 'sending')]))
      .toEqual([{ prompt: '二', reply: '答二' }])
  })

  it('is due after the first answered turn, or later while the prompts are too short, by the third', () => {
    expect(titleDue([])).toBe(false)
    expect(titleDue([{ prompt: '帮我写周报', reply: 'x' }])).toBe(true)
    expect(titleDue([{ prompt: '你好', reply: 'x' }])).toBe(false)
    expect(titleDue([{ prompt: ' h i ', reply: 'x' }])).toBe(false)
    expect(titleDue([{ prompt: '你好', reply: 'x' }, { prompt: '写周报吧', reply: 'y' }])).toBe(true)
    expect(titleDue([{ prompt: '你好', reply: 'x' }, { prompt: '嗯', reply: 'y' }])).toBe(false)
    expect(titleDue([{ prompt: '你好', reply: 'x' }, { prompt: '嗯', reply: 'y' }, { prompt: '好', reply: 'z' }])).toBe(true)
  })

  it('asks without session_id, with the turns cut to 1000 characters in all', () => {
    const body = titleRequest([{ prompt: '问'.repeat(800), reply: '答'.repeat(800) }])
    expect(body).not.toHaveProperty('session_id')
    expect(body).not.toHaveProperty('idoris_headroom_confirmed')
    expect(body.messages.map(m => m.role)).toEqual(['system', 'user'])
    expect(body.messages[1].content).toBe(`User: ${'问'.repeat(500)}…\nAssistant: ${'答'.repeat(500)}…`)
    const three = titleRequest([1, 2, 3].map(n => ({ prompt: `问${n}`, reply: `答${n}` })))
    expect(three.messages[1].content).toBe('User: 问1\nAssistant: 答1\n\nUser: 问2\nAssistant: 答2\n\nUser: 问3\nAssistant: 答3')
  })
})
