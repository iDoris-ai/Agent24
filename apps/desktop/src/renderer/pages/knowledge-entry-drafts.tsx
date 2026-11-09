// KB-SOP-01b-2 (#1669): local knowledge-entry drafts on the Knowledge page.
// Main owns the files (main/knowledge-entry-ipc.ts). This panel only shows
// them, edits the title, kind and body, and can redact or delete one.
// H2 (#1670): submit one for review, check its status, withdraw the
// submission. The panel names a draft id and one knowledge base out of the
// binding's own list; main builds and judges everything else
// (main/knowledge-entry-submit.ts). Deleting the local draft never touches a
// submission; withdrawing the submission keeps the local draft.

import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore, type ReactNode } from 'react'
import type {
  KnowledgeEntryDraftSaved,
  KnowledgeEntryDraftSummary,
  KnowledgeEntryDraftView,
  KnowledgeEntryFinding,
  KnowledgeEntryKind,
  KnowledgeEntryModelOutcome,
  KnowledgeEntrySubmission,
  KnowledgeEntrySubmitAnswer,
  KnowledgeEntrySubmitRefusal,
} from '../../shared/ipc-types'
import { useLeaveGuard } from '../capabilities/providers'
import { defineMessages, useT, type T } from '../i18n'

const draftMessages = defineMessages({
  en: {
    'list.aria': 'Knowledge entry drafts',
    'bar.count': 'Knowledge entry drafts: {n}',
    'bar.view': 'View',
    'kind.sop': 'Steps',
    'kind.faq': 'Q&A',
    'kind.note': 'Note',
    'badge.changed': 'Needs a new target library',
    'badge.none': 'Knowledge is off',
    'badge.findings': 'May contain sensitive content',
    'field.kind': 'Type',
    'field.title': 'Title',
    'field.body': 'Body',
    'detail.source': 'From messages {from}–{to} of the conversation',
    'detail.missing': 'This draft is no longer here.',
    'detail.targetChanged': 'Needs a new target library: Knowledge is no longer bound to the knowledge base this draft was created for, and this draft will not switch over on its own.',
    'detail.targetNone': 'Knowledge is off or not bound to a knowledge base.',
    'detail.findings': 'May contain sensitive content: {categories}. Edit it before submitting, or choose “Redact”.',
    'finding.private_key': 'private key',
    'finding.credential': 'credential',
    'finding.email': 'email',
    'finding.phone': 'phone',
    'finding.id_card': 'ID number',
    'finding.bank_card': 'bank card',
    'finding.path': 'path',
    'finding.internal_address': 'internal address',
    'model.no_model': 'No on-device model is available. An empty template is ready for you to fill in.',
    'model.needs_confirmation': 'The on-device model needs you to confirm its resource use first. An empty template is ready. You can confirm in the conversation and try again.',
    'model.busy': 'The on-device model is busy. An empty template is ready.',
    'model.failed': 'The on-device model did not answer. An empty template is ready.',
    'model.unusable': 'The on-device model’s answer could not be used. An empty template is ready.',
    'model.held_back': 'Some private details in the chosen messages could not be hidden automatically, so they were not given to the on-device model. An empty template is ready.',
    'detail.hints': 'The model suggests another look (a hint only; it does not affect submitting): {hints}',
    'action.save': 'Save',
    'action.redact': 'Redact',
    'action.withdraw': 'Delete draft',
    'action.submit': 'Submit for review',
    'action.resubmit': 'Try submitting again',
    'action.status': 'Check status',
    'action.retract': 'Withdraw submission',
    'action.back': 'Back to list',
    'action.close': 'Close',
    'save.invalid': 'Could not save: the title cannot be empty, and the body cannot exceed 64 KiB.',
    'save.not_saved': 'The draft could not be saved. Try again in a moment.',
    'redact.too_long': 'Redacting would make the body longer than 64 KiB. Shorten it by hand first.',
    'withdraw.confirm': 'Delete this draft from this machine? It cannot be restored. A submission already sent for review stays there; withdraw it first if you want it gone.',
    'withdraw.failed': 'Could not delete the draft. Try again in a moment.',
    'submit.changed': 'Needs a new target library',
    'submit.findings': 'Handle the possible sensitive content first',
    'submit.none': 'Turn on Knowledge and bind a knowledge base first',
    'submit.saveFirst': 'Save your changes first',
    'submit.format': 'Line {line} uses formatting the knowledge base does not accept (such as HTML, images, footnotes or a link that is not a plain https address). Edit it before submitting.',
    'submit.target': 'Knowledge base',
    'status.pending': 'Sent, but whether it arrived is not known yet. Trying again sends the same submission, never a second one.',
    'status.stuck': 'Whether this submission arrived cannot be told any more: Knowledge’s connection or key changed, or too much time has passed. This draft cannot be submitted again. Ask the knowledge base’s reviewers whether they received it; if not, make a new draft and submit that.',
    'status.submitted': 'Submitted for review to {kb}.',
    'status.approved': 'Approved by the reviewers.',
    'status.returned': 'Returned by the reviewers: {reason}.',
    'status.withdrawn': 'You withdrew this submission. You can submit the draft again.',
    'status.note': 'Reviewer’s note: {note}',
    'status.edited': 'Edited since it was submitted.',
    'status.checked': 'Last checked {time}.',
    'return.reviewer': 'a reviewer sent it back',
    'return.conflicted': 'it conflicts with an existing entry',
    'return.target_withdrawn': 'the knowledge base stopped taking entries',
    'return.target_gone': 'the knowledge base no longer exists',
    'return.unsupported_format': 'it uses formatting the knowledge base does not accept',
    'return.too_large': 'it is too long',
    'return.other': 'another reason',
    'retract.confirm': 'Withdraw this submission from review? The draft stays on this machine.',
    'error.permission': 'The knowledge base refused: this key may not submit entries to it. Check Knowledge’s settings.',
    'error.unavailable': 'The knowledge base is not taking submissions right now. Try again later.',
    'error.rejected': 'The knowledge base did not accept this request. Check the draft status, then try again.',
    'error.failed': 'Something went wrong talking to the knowledge base. Nothing was changed; try again in a moment.',
    'error.unknown': 'No clear answer came back, so the submission may or may not have arrived. Trying again is safe: it sends the same submission.',
    'error.quota': 'Too many of your entries are waiting for review. Submit again once the reviewers have dealt with some.',
    'error.empty_scope': 'This key has no knowledge base it may submit to. Check Knowledge’s settings.',
    'error.format': 'The knowledge base does not accept some formatting in the body (such as HTML, images, footnotes or a link that is not a plain https address). Edit it before submitting.',
    'error.too_large': 'The body is longer than the knowledge base accepts (64 KiB). Shorten it before submitting.',
    'error.busy': 'This draft is already being submitted.',
    'error.local_storage': 'This computer could not read or write the submission record, so nothing was sent to the knowledge base. Try again in a moment.',
    'error.local_record': 'The knowledge base handled the request, but this computer could not record the result. Do it again in a moment to catch up: nothing is submitted or withdrawn twice.',
    'error.already': 'This draft is already under review or approved.',
    'error.invalid_target': 'Choose one of the knowledge bases Knowledge is bound to.',
    'confirm.discard': 'There are unsaved changes. Discard them?',
    'join.sep': ', ',
  },
  zh: {
    'list.aria': '知识条目草稿',
    'bar.count': '知识条目草稿：{n} 份',
    'bar.view': '查看',
    'kind.sop': '操作步骤',
    'kind.faq': '问答',
    'kind.note': '笔记',
    'badge.changed': '需要重新选目标库',
    'badge.none': 'Knowledge 未开启',
    'badge.findings': '含疑似敏感内容',
    'field.kind': '类型',
    'field.title': '标题',
    'field.body': '正文',
    'detail.source': '来自对话第 {from}–{to} 条消息',
    'detail.missing': '这份草稿已经不在了。',
    'detail.targetChanged': '需要重新选目标库：Knowledge 现在绑定的不是建草稿时的知识库，这份草稿不会自动改投。',
    'detail.targetNone': 'Knowledge 未开启或未绑定知识库。',
    'detail.findings': '含疑似敏感内容：{categories}。提交前请删改，或点「一键脱敏」。',
    'finding.private_key': '私钥',
    'finding.credential': '凭据',
    'finding.email': '邮箱',
    'finding.phone': '电话',
    'finding.id_card': '证件号',
    'finding.bank_card': '银行卡号',
    'finding.path': '路径',
    'finding.internal_address': '内网地址',
    'model.no_model': '没有可用的本机模型，已给出空模板，请自己填写。',
    'model.needs_confirmation': '本机模型需要先确认资源占用，已给出空模板。可以在对话里确认后再试。',
    'model.busy': '本机模型正忙，已给出空模板。',
    'model.failed': '本机模型没有答复，已给出空模板。',
    'model.unusable': '本机模型的答复无法使用，已给出空模板。',
    'model.held_back': '所选消息里有无法自动脱敏的内容，没有交给本机模型，已给出空模板。',
    'detail.hints': '模型提示再看一眼（只是提示，不影响提交）：{hints}',
    'action.save': '保存',
    'action.redact': '一键脱敏',
    'action.withdraw': '删除草稿',
    'action.submit': '提交审阅',
    'action.resubmit': '重新提交',
    'action.status': '查看状态',
    'action.retract': '撤回提交',
    'action.back': '返回列表',
    'action.close': '关闭',
    'save.invalid': '没能保存：标题不能为空，正文不能超过 64 KiB。',
    'save.not_saved': '草稿没能保存，请稍后再试。',
    'redact.too_long': '脱敏后正文会超过 64 KiB，请先手工删减。',
    'withdraw.confirm': '确定从本机删除这份草稿？删除后不能恢复。已经提交审阅的内容不会因此撤回，需要的话请先撤回提交。',
    'withdraw.failed': '没能删除草稿，请稍后再试。',
    'submit.changed': '需要重新选目标库',
    'submit.findings': '请先处理疑似敏感内容',
    'submit.none': '请先开启 Knowledge 并绑定知识库',
    'submit.saveFirst': '请先保存修改',
    'submit.format': '第 {line} 行用了知识库不接受的格式（例如 HTML、图片、脚注，或不是普通 https 地址的链接），请修改后再提交。',
    'submit.target': '知识库',
    'status.pending': '已发出，但还不知道是否送达。重新提交发出的是同一份提交，不会多出一份。',
    'status.stuck': '已经无法确认这次提交是否送达：Knowledge 的连接或密钥变了，或者时间过去太久。这份草稿不能再提交了。请向知识库的审阅人确认是否收到；没有收到的话，请新建一份草稿再提交。',
    'status.submitted': '已提交到 {kb} 等待审阅。',
    'status.approved': '审阅已通过。',
    'status.returned': '审阅人已退回：{reason}。',
    'status.withdrawn': '你已撤回这次提交，可以重新提交这份草稿。',
    'status.note': '审阅人备注：{note}',
    'status.edited': '提交之后改动过。',
    'status.checked': '上次查看：{time}。',
    'return.reviewer': '审阅人退回',
    'return.conflicted': '与已有条目冲突',
    'return.target_withdrawn': '目标知识库已停止接收',
    'return.target_gone': '目标知识库已不存在',
    'return.unsupported_format': '用了知识库不接受的格式',
    'return.too_large': '内容过长',
    'return.other': '其他原因',
    'retract.confirm': '确定撤回这次提交？草稿仍保留在本机。',
    'error.permission': '知识库拒绝了：这个密钥无权向它提交条目。请检查 Knowledge 设置。',
    'error.unavailable': '知识库现在不接收提交，请稍后再试。',
    'error.rejected': '知识库没有接受这次请求。请先查看草稿状态，再重试。',
    'error.failed': '和知识库通信时出错，没有做任何改动，请稍后再试。',
    'error.unknown': '没有收到明确答复，提交可能送达也可能没有。重新提交是安全的，发出的是同一份提交。',
    'error.quota': '你待审的条目已满，等审阅人处理一些之后再提交。',
    'error.empty_scope': '这个密钥没有可以提交的知识库范围，请检查 Knowledge 设置。',
    'error.format': '知识库不接受正文里的部分格式（例如 HTML、图片、脚注，或不是普通 https 地址的链接），请修改后再提交。',
    'error.too_large': '正文超过知识库接受的长度（64 KiB），请删减后再提交。',
    'error.busy': '这份草稿正在提交中。',
    'error.local_storage': '本机没能读写提交记录，这次没有发给知识库。请稍后再试。',
    'error.local_record': '知识库已经处理了这次请求，但本机没能记下结果。稍后再操作一次就能补上，不会重复提交或重复撤回。',
    'error.already': '这份草稿已在审阅中或已通过。',
    'error.invalid_target': '请选择 Knowledge 已绑定的知识库之一。',
    'confirm.discard': '有未保存的修改，确定放弃？',
    'join.sep': '、',
  },
  th: {
    'list.aria': 'ฉบับร่างรายการความรู้',
    'bar.count': 'ฉบับร่างรายการความรู้: {n} ฉบับ',
    'bar.view': 'ดู',
    'kind.sop': 'ขั้นตอน',
    'kind.faq': 'ถามตอบ',
    'kind.note': 'บันทึก',
    'badge.changed': 'ต้องเลือกคลังเป้าหมายใหม่',
    'badge.none': 'Knowledge ปิดอยู่',
    'badge.findings': 'อาจมีเนื้อหาที่ละเอียดอ่อน',
    'field.kind': 'ประเภท',
    'field.title': 'ชื่อเรื่อง',
    'field.body': 'เนื้อหา',
    'detail.source': 'จากข้อความที่ {from}–{to} ของบทสนทนา',
    'detail.missing': 'ไม่พบฉบับร่างนี้แล้ว',
    'detail.targetChanged': 'ต้องเลือกคลังเป้าหมายใหม่: Knowledge ที่เชื่อมอยู่ตอนนี้ไม่ใช่คลังที่ใช้ตอนสร้างฉบับร่าง ฉบับร่างนี้จะไม่ถูกย้ายให้เอง',
    'detail.targetNone': 'Knowledge ปิดอยู่หรือยังไม่ได้เชื่อมกับฐานความรู้',
    'detail.findings': 'อาจมีเนื้อหาที่ละเอียดอ่อน: {categories} โปรดแก้ไขก่อนส่ง หรือกด «ปิดบังข้อมูล»',
    'finding.private_key': 'กุญแจส่วนตัว',
    'finding.credential': 'ข้อมูลรับรอง',
    'finding.email': 'อีเมล',
    'finding.phone': 'โทรศัพท์',
    'finding.id_card': 'เลขบัตร',
    'finding.bank_card': 'เลขบัตรธนาคาร',
    'finding.path': 'พาธ',
    'finding.internal_address': 'ที่อยู่ในเครือข่ายภายใน',
    'model.no_model': 'ไม่มีโมเดลในเครื่องที่ใช้ได้ จึงให้เทมเพลตว่างไว้ โปรดกรอกเอง',
    'model.needs_confirmation': 'โมเดลในเครื่องต้องยืนยันการใช้ทรัพยากรก่อน จึงให้เทมเพลตว่างไว้ สามารถยืนยันในบทสนทนาแล้วลองใหม่',
    'model.busy': 'โมเดลในเครื่องไม่ว่าง จึงให้เทมเพลตว่างไว้',
    'model.failed': 'โมเดลในเครื่องไม่ตอบ จึงให้เทมเพลตว่างไว้',
    'model.unusable': 'คำตอบของโมเดลในเครื่องใช้ไม่ได้ จึงให้เทมเพลตว่างไว้',
    'model.held_back': 'ข้อความที่เลือกมีข้อมูลส่วนตัวที่ปิดบังอัตโนมัติไม่ได้ จึงไม่ได้ส่งให้โมเดลในเครื่อง และให้เทมเพลตว่างไว้',
    'detail.hints': 'โมเดลแนะนำให้ดูอีกครั้ง (เป็นเพียงคำแนะนำ ไม่กระทบการส่ง): {hints}',
    'action.save': 'บันทึก',
    'action.redact': 'ปิดบังข้อมูล',
    'action.withdraw': 'ลบฉบับร่าง',
    'action.submit': 'ส่งให้ตรวจ',
    'action.resubmit': 'ส่งอีกครั้ง',
    'action.status': 'ดูสถานะ',
    'action.retract': 'ถอนการส่ง',
    'action.back': 'กลับไปที่รายการ',
    'action.close': 'ปิด',
    'save.invalid': 'บันทึกไม่สำเร็จ: ชื่อเรื่องต้องไม่ว่าง และเนื้อหาต้องไม่เกิน 64 KiB',
    'save.not_saved': 'บันทึกฉบับร่างไม่สำเร็จ โปรดลองอีกครั้งในอีกสักครู่',
    'redact.too_long': 'หากปิดบังข้อมูล เนื้อหาจะยาวเกิน 64 KiB โปรดตัดให้สั้นลงเองก่อน',
    'withdraw.confirm': 'ลบฉบับร่างนี้ออกจากเครื่องนี้หรือไม่ กู้คืนไม่ได้ สิ่งที่ส่งให้ตรวจไปแล้วจะยังอยู่ ถ้าต้องการให้หายไปด้วย โปรดถอนการส่งก่อน',
    'withdraw.failed': 'ลบฉบับร่างไม่สำเร็จ โปรดลองอีกครั้งในอีกสักครู่',
    'submit.changed': 'ต้องเลือกคลังเป้าหมายใหม่',
    'submit.findings': 'โปรดจัดการเนื้อหาที่อาจละเอียดอ่อนก่อน',
    'submit.none': 'โปรดเปิด Knowledge และผูกคลังความรู้ก่อน',
    'submit.saveFirst': 'โปรดบันทึกการแก้ไขก่อน',
    'submit.format': 'บรรทัดที่ {line} ใช้รูปแบบที่คลังความรู้ไม่รับ (เช่น HTML รูปภาพ เชิงอรรถ หรือลิงก์ที่ไม่ใช่ที่อยู่ https ธรรมดา) โปรดแก้ไขก่อนส่ง',
    'submit.target': 'คลังความรู้',
    'status.pending': 'ส่งแล้ว แต่ยังไม่รู้ว่าถึงหรือไม่ การส่งอีกครั้งจะส่งรายการเดิม ไม่เกิดรายการที่สอง',
    'status.stuck': 'บอกไม่ได้แล้วว่าการส่งครั้งนี้ถึงหรือไม่ เพราะการเชื่อมต่อหรือคีย์ของ Knowledge เปลี่ยน หรือเวลาผ่านไปนานเกินไป ฉบับร่างนี้ส่งอีกไม่ได้แล้ว โปรดสอบถามผู้ตรวจของคลังความรู้ว่าได้รับหรือไม่ หากยังไม่ได้รับ โปรดสร้างฉบับร่างใหม่แล้วส่งฉบับนั้น',
    'status.submitted': 'ส่งให้ {kb} ตรวจแล้ว',
    'status.approved': 'ผู้ตรวจอนุมัติแล้ว',
    'status.returned': 'ผู้ตรวจส่งคืน: {reason}',
    'status.withdrawn': 'คุณถอนการส่งนี้แล้ว ส่งฉบับร่างอีกครั้งได้',
    'status.note': 'หมายเหตุจากผู้ตรวจ: {note}',
    'status.edited': 'แก้ไขหลังจากส่งแล้ว',
    'status.checked': 'ดูล่าสุด {time}',
    'return.reviewer': 'ผู้ตรวจส่งคืน',
    'return.conflicted': 'ขัดกับรายการที่มีอยู่',
    'return.target_withdrawn': 'คลังความรู้หยุดรับรายการแล้ว',
    'return.target_gone': 'คลังความรู้ไม่มีอยู่แล้ว',
    'return.unsupported_format': 'ใช้รูปแบบที่คลังความรู้ไม่รับ',
    'return.too_large': 'ยาวเกินไป',
    'return.other': 'เหตุผลอื่น',
    'retract.confirm': 'ถอนการส่งนี้ออกจากการตรวจหรือไม่ ฉบับร่างยังอยู่ในเครื่องนี้',
    'error.permission': 'คลังความรู้ปฏิเสธ: คีย์นี้ไม่มีสิทธิ์ส่งรายการไปที่นั่น โปรดตรวจการตั้งค่า Knowledge',
    'error.unavailable': 'คลังความรู้ยังไม่รับการส่งในตอนนี้ โปรดลองใหม่ภายหลัง',
    'error.rejected': 'คลังความรู้ไม่รับคำขอนี้ โปรดดูสถานะฉบับร่างก่อนแล้วลองใหม่',
    'error.failed': 'เกิดข้อผิดพลาดระหว่างติดต่อคลังความรู้ ไม่มีอะไรเปลี่ยนแปลง โปรดลองอีกครั้งในอีกสักครู่',
    'error.unknown': 'ไม่ได้รับคำตอบที่ชัดเจน การส่งอาจถึงหรือไม่ถึงก็ได้ การส่งอีกครั้งปลอดภัย เพราะจะส่งรายการเดิม',
    'error.quota': 'รายการของคุณที่รอตรวจเต็มแล้ว โปรดส่งอีกครั้งหลังผู้ตรวจจัดการไปบางส่วน',
    'error.empty_scope': 'คีย์นี้ไม่มีคลังความรู้ที่ส่งได้ โปรดตรวจการตั้งค่า Knowledge',
    'error.format': 'คลังความรู้ไม่รับรูปแบบบางอย่างในเนื้อหา (เช่น HTML รูปภาพ เชิงอรรถ หรือลิงก์ที่ไม่ใช่ที่อยู่ https ธรรมดา) โปรดแก้ไขก่อนส่ง',
    'error.too_large': 'เนื้อหายาวเกินกว่าที่คลังความรู้รับ (64 KiB) โปรดตัดให้สั้นลงก่อนส่ง',
    'error.busy': 'ฉบับร่างนี้กำลังถูกส่งอยู่',
    'error.local_storage': 'เครื่องนี้อ่านหรือเขียนบันทึกการส่งไม่ได้ ครั้งนี้จึงไม่ได้ส่งไปที่คลังความรู้ โปรดลองอีกครั้งในอีกสักครู่',
    'error.local_record': 'คลังความรู้ดำเนินการคำขอแล้ว แต่เครื่องนี้บันทึกผลไม่ได้ ทำอีกครั้งในอีกสักครู่เพื่อให้ตรงกัน จะไม่ส่งหรือถอนซ้ำ',
    'error.already': 'ฉบับร่างนี้อยู่ระหว่างตรวจหรืออนุมัติแล้ว',
    'error.invalid_target': 'โปรดเลือกคลังความรู้ที่ Knowledge ผูกไว้',
    'confirm.discard': 'มีการแก้ไขที่ยังไม่บันทึก ยืนยันที่จะทิ้งหรือไม่',
    'join.sep': ', ',
  },
})

type Copy = T<keyof typeof draftMessages.en>
type CreatedNote = { model: KnowledgeEntryModelOutcome; hints: string[] }
type PanelState =
  | null
  | { view: 'list' }
  | { view: 'detail'; id: string; created?: CreatedNote }

const FINDING_KEY: Record<KnowledgeEntryFinding, 'finding.private_key' | 'finding.credential' | 'finding.email' | 'finding.phone' | 'finding.id_card' | 'finding.bank_card' | 'finding.path' | 'finding.internal_address'> = {
  private_key: 'finding.private_key',
  credential: 'finding.credential',
  email: 'finding.email',
  phone: 'finding.phone',
  id_card: 'finding.id_card',
  bank_card: 'finding.bank_card',
  path: 'finding.path',
  internal_address: 'finding.internal_address',
}

let panel: PanelState = null
const listeners = new Set<() => void>()
/** Set by the open draft while it has unsaved edits: asks the user, true when leaving is fine. */
let leaveGuard: (() => boolean) | null = null

function publish(next: PanelState): void {
  panel = next
  for (const listener of listeners) listener()
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => { listeners.delete(listener) }
}

/**
 * Opens one draft. `extra` is the model step from the create that just
 * finished; only that opening shows it. Another draft with unsaved edits is
 * open: the user is asked first, and on no it stays (the new draft is in the
 * list).
 */
export function openDraft(id: string, extra?: CreatedNote): void {
  if (panel?.view === 'detail' && panel.id !== id && leaveGuard && !leaveGuard()) return
  publish({ view: 'detail', id, created: extra })
}

export function openDraftList(): void {
  publish({ view: 'list' })
}

export function closeDrafts(): void {
  publish(null)
}

export function useDraftsPanel(): PanelState {
  return useSyncExternalStore(subscribe, () => panel)
}

function invoke<T>(run: () => Promise<T>): Promise<T> {
  try {
    return Promise.resolve(run())
  } catch (err) {
    return Promise.reject(err)
  }
}

function isFinding(value: string): value is KnowledgeEntryFinding {
  return Object.hasOwn(FINDING_KEY, value)
}

function isKind(value: unknown): value is KnowledgeEntryKind {
  return value === 'sop' || value === 'faq' || value === 'note'
}

function isDraftView(value: unknown): value is KnowledgeEntryDraftView {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Partial<KnowledgeEntryDraftView>
  return typeof v.id === 'string'
    && isKind(v.kind)
    && typeof v.title === 'string'
    && typeof v.body === 'string'
    && (v.target === 'same' || v.target === 'changed' || v.target === 'none')
    && Array.isArray(v.findings)
    && v.findings.every((item) => typeof item === 'string')
    && typeof v.message_from === 'number'
    && typeof v.message_to === 'number'
    && isSubmission(v.submission)
    && Array.isArray(v.knowledge_base_ids)
    && v.knowledge_base_ids.every((item) => typeof item === 'string')
    && (v.format === null || (typeof v.format === 'object' && v.format !== undefined && typeof v.format.line === 'number'))
}

function isSummary(value: unknown): value is KnowledgeEntryDraftSummary {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Partial<KnowledgeEntryDraftSummary>
  return typeof v.id === 'string'
    && isKind(v.kind)
    && typeof v.title === 'string'
    && (v.target === 'same' || v.target === 'changed' || v.target === 'none')
    && Array.isArray(v.findings)
    && v.findings.every((item) => typeof item === 'string')
}

function isSaved(value: unknown): value is KnowledgeEntryDraftSaved {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown; draft?: unknown }
  if (v.ok === true) return isDraftView(v.draft)
  return v.ok === false && (v.reason === 'not_found' || v.reason === 'invalid' || v.reason === 'not_saved' || v.reason === 'too_long')
}

const STATUSES = new Set(['submitted', 'approved', 'returned', 'withdrawn'])
const RETURN_KEY = {
  reviewer: 'return.reviewer',
  conflicted: 'return.conflicted',
  target_withdrawn: 'return.target_withdrawn',
  target_gone: 'return.target_gone',
  unsupported_format: 'return.unsupported_format',
  too_large: 'return.too_large',
} as const

function isSubmission(value: unknown): value is KnowledgeEntrySubmission {
  if (typeof value !== 'object' || value === null) return false
  const v = value as Partial<KnowledgeEntrySubmission>
  if (typeof v.pending !== 'boolean' || typeof v.stuck !== 'boolean') return false
  const c = v.candidate
  if (c === null) return true
  return typeof c === 'object' && c !== undefined
    && typeof c.status === 'string' && STATUSES.has(c.status)
    && typeof c.target_knowledge_base_id === 'string'
    && typeof c.edited === 'boolean'
    && (c.return_reason === null || typeof c.return_reason === 'string')
    && (c.note === null || typeof c.note === 'string')
    && typeof c.checked_at === 'string'
}

const REFUSALS: ReadonlySet<string> = new Set<KnowledgeEntrySubmitRefusal>([
  'not_found', 'target_changed', 'invalid_target', 'findings', 'unsupported_format',
  'already_submitted', 'already_approved', 'busy', 'local_storage', 'knowledge_off', 'outcome_unknown',
])

function isSubmitAnswer(value: unknown): value is KnowledgeEntrySubmitAnswer {
  if (typeof value !== 'object' || value === null) return false
  const v = value as { ok?: unknown; reason?: unknown; submission?: unknown; code?: unknown; unknown?: unknown; serverReason?: unknown; problem?: unknown }
  if (v.ok === true) return isSubmission(v.submission)
  if (v.ok !== false || typeof v.reason !== 'string') return false
  if (v.reason === 'server') {
    return typeof v.code === 'string' && typeof v.unknown === 'boolean' && isSubmission(v.submission)
      && (v.serverReason === undefined || typeof v.serverReason === 'string')
  }
  if (!REFUSALS.has(v.reason)) return false
  if (v.problem !== undefined) {
    const problem = v.problem as { kind?: unknown; line?: unknown } | null
    if (typeof problem !== 'object' || problem === null || typeof problem.kind !== 'string'
      || typeof problem.line !== 'number' || !Number.isSafeInteger(problem.line) || problem.line < 1) return false
  }
  return v.submission === undefined || isSubmission(v.submission)
}

/**
 * HS5: the five kinds of server answer the user is told apart, and the write
 * reasons with their own advice. The words are this file's own; nothing from
 * the answer (and never the body or the key) is shown. A reason counts only
 * under the code WeKnora sends it with (CONTRACT-OPS-v2 §6): a `too_large`
 * under `internal` is main's own "answer too long to read", not the body.
 */
function serverError(t: Copy, code: string, unknown: boolean, reason: string | undefined): string {
  if (unknown) return t('error.unknown')
  if (code === 'forbidden' && reason === 'quota_exceeded') return t('error.quota')
  if (code === 'forbidden' && reason === 'empty_scope') return t('error.empty_scope')
  if (code === 'invalid_request' && reason === 'unsupported_format') return t('error.format')
  if (code === 'invalid_request' && reason === 'too_large') return t('error.too_large')
  if (code === 'unauthorized' || code === 'forbidden') return t('error.permission')
  if (code === 'unavailable' || code === 'not_ready') return t('error.unavailable')
  if (code === 'invalid_request' || code === 'stale' || code === 'not_found' || code === 'conflict') return t('error.rejected')
  return t('error.failed')
}

function refusal(t: Copy, answer: Exclude<KnowledgeEntrySubmitAnswer, { ok: true }>): string {
  switch (answer.reason) {
    case 'server': return serverError(t, answer.code, answer.unknown, answer.serverReason)
    case 'busy': return t('error.busy')
    // In H1b a local_storage answer carries the submission exactly when the
    // server had already acted and only recording the result failed; without
    // it, nothing went out.
    case 'local_storage': return t(answer.submission ? 'error.local_record' : 'error.local_storage')
    case 'outcome_unknown': return t('status.stuck')
    case 'already_submitted':
    case 'already_approved': return t('error.already')
    case 'invalid_target': return t('error.invalid_target')
    case 'target_changed': return t('detail.targetChanged')
    case 'knowledge_off': return t('submit.none')
    case 'findings': return t('submit.findings')
    case 'unsupported_format': return t('submit.format', { line: answer.problem?.line ?? 1 })
    case 'not_found': return t('detail.missing')
    default: return t('error.failed')
  }
}

function when(iso: string): string {
  const at = Date.parse(iso)
  return Number.isNaN(at) ? iso : new Date(at).toLocaleString()
}

function kindLabel(t: Copy, kind: KnowledgeEntryKind): string {
  if (kind === 'sop') return t('kind.sop')
  if (kind === 'faq') return t('kind.faq')
  return t('kind.note')
}

function itemLabel(t: Copy, item: KnowledgeEntryDraftSummary): string {
  const badges: string[] = []
  if (item.target === 'changed') badges.push(t('badge.changed'))
  if (item.target === 'none') badges.push(t('badge.none'))
  if (item.findings.length > 0) badges.push(t('badge.findings'))
  const kind = kindLabel(t, item.kind)
  return badges.length > 0 ? `${item.title} ${kind} ${badges.join(' ')}` : `${item.title} ${kind}`
}

function modelLine(t: Copy, model: KnowledgeEntryModelOutcome): string | null {
  switch (model) {
    case 'drafted': return null
    case 'no_model': return t('model.no_model')
    case 'needs_confirmation': return t('model.needs_confirmation')
    case 'busy': return t('model.busy')
    case 'failed': return t('model.failed')
    case 'unusable': return t('model.unusable')
    case 'held_back': return t('model.held_back')
  }
}

function Shell({ children }: { children?: ReactNode }): JSX.Element {
  const t = useT(draftMessages)
  const hidden = useContext(HiddenContext)
  // `.knowledge-confirm` sets `display: flex`, which beats the `[hidden]`
  // default, so `hidden` alone leaves the panel on screen and focusable.
  // The inline `display: none` is what takes it away.
  return (
    <section
      className="knowledge-confirm"
      role="dialog"
      aria-modal="false"
      aria-label={t('list.aria')}
      hidden={hidden}
      style={hidden ? { display: 'none' } : undefined}
    >
      {children}
    </section>
  )
}

function DraftList(): JSX.Element {
  const t = useT(draftMessages)
  const [items, setItems] = useState<KnowledgeEntryDraftSummary[] | null>(null)
  useEffect(() => {
    let live = true
    const list = window.agent24?.knowledgeEntry?.listDrafts
    if (typeof list !== 'function') {
      setItems([])
      return
    }
    invoke(() => list()).then(
      (value) => { if (live) setItems(Array.isArray(value) ? value.filter(isSummary) : []) },
      () => { if (live) setItems([]) },
    )
    return () => { live = false }
  }, [])
  if (items === null) return <Shell />
  return (
    <Shell>
      <p className="knowledge-confirm-count">{t('bar.count', { n: items.length })}</p>
      <ul className="knowledge-confirm-titles">
        {items.map((item) => (
          <li key={item.id}>
            <button type="button" onClick={() => openDraft(item.id)}>{itemLabel(t, item)}</button>
          </li>
        ))}
      </ul>
      <button className="btn" type="button" onClick={() => closeDrafts()}>{t('action.close')}</button>
    </Shell>
  )
}

function DraftDetail({ id, created }: { id: string; created?: CreatedNote }): JSX.Element {
  const t = useT(draftMessages)
  const [draft, setDraft] = useState<KnowledgeEntryDraftView | null | undefined>(undefined)
  const [kind, setKind] = useState<KnowledgeEntryKind>('note')
  const [title, setTitle] = useState('')
  const [body, setBody] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const [targetKb, setTargetKb] = useState('')
  const flight = useRef(false)
  const mounted = useRef(true)
  // Set again on every setup: StrictMode runs setup, cleanup, setup.
  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])

  useEffect(() => {
    let live = true
    const get = window.agent24?.knowledgeEntry?.getDraft
    if (typeof get !== 'function') {
      setDraft(null)
      return
    }
    invoke(() => get(id)).then(
      (value) => {
        if (!live) return
        if (!isDraftView(value)) { setDraft(null); return }
        setKind(value.kind)
        setTitle(value.title)
        setBody(value.body)
        setError(null)
        setDraft(value)
      },
      () => { if (live) setDraft(null) },
    )
    return () => { live = false }
  }, [id])

  const applySaved = (value: unknown, fallback: string): void => {
    if (!isSaved(value)) { setError(fallback); return }
    if (!value.ok) {
      setError(value.reason === 'invalid' ? t('save.invalid')
        : value.reason === 'not_found' ? t('detail.missing')
          : value.reason === 'too_long' ? t('redact.too_long')
            : t('save.not_saved'))
      return
    }
    setKind(value.draft.kind)
    setTitle(value.draft.title)
    setBody(value.draft.body)
    setDraft(value.draft)
    setError(null)
  }

  const run = (task: () => Promise<unknown>, done: (value: unknown) => void, fail: () => void): void => {
    if (flight.current) return
    flight.current = true
    setBusy(true)
    setError(null)
    invoke(task).then(
      (value) => {
        flight.current = false
        if (!mounted.current) return
        setBusy(false)
        done(value)
      },
      () => {
        flight.current = false
        if (!mounted.current) return
        setBusy(false)
        fail()
      },
    )
  }

  const dirty = draft ? kind !== draft.kind || title !== draft.title || body !== draft.body : false
  // Someone outside the panel (a new draft opening) asks before these edits go.
  useEffect(() => {
    leaveGuard = dirty ? () => window.confirm(t('confirm.discard')) : null
    return () => { leaveGuard = null }
  }, [dirty, t])
  // M4 A7: going to another page asks too, once, with the same words.
  useLeaveGuard(dirty, t('confirm.discard'))

  if (draft === undefined) return <Shell />
  if (draft === null) {
    return (
      <Shell>
        <p role="status">{t('detail.missing')}</p>
        <button className="btn" type="button" onClick={() => openDraftList()}>{t('action.back')}</button>
        <button className="btn" type="button" onClick={() => closeDrafts()}>{t('action.close')}</button>
      </Shell>
    )
  }

  const categories = draft.findings.filter(isFinding).map((finding) => t(FINDING_KEY[finding])).join(t('join.sep'))
  const createdModel = created ? modelLine(t, created.model) : null
  const createdHints = created && created.hints.length > 0
    ? t('detail.hints', { hints: created.hints.join(t('join.sep')) })
    : null
  const submission = draft.submission
  const candidate = submission.candidate
  // Open on the server: not to be submitted again until it is decided or withdrawn.
  const open = candidate?.status === 'submitted' || candidate?.status === 'approved'
  const kbs = draft.knowledge_base_ids
  const chosenKb = kbs.includes(targetKb) ? targetKb : (kbs[0] ?? '')
  // Why a new submission cannot go now (main judges every one of these again).
  const blocker = draft.target === 'none' ? t('submit.none')
    : draft.target === 'changed' ? t('submit.changed')
      : draft.findings.length > 0 ? t('submit.findings')
        : draft.format ? t('submit.format', { line: draft.format.line })
          : dirty ? t('submit.saveFirst')
            : kbs.length === 0 ? t('submit.none')
              : null
  // A pending submit goes again exactly as it was stored: edits do not matter.
  const resend = submission.pending && !submission.stuck
  const canSubmit = !submission.pending && !open
  const statusLines: string[] = []
  if (submission.pending) statusLines.push(t(submission.stuck ? 'status.stuck' : 'status.pending'))
  if (candidate) {
    if (candidate.status === 'submitted') statusLines.push(t('status.submitted', { kb: candidate.target_knowledge_base_id }))
    if (candidate.status === 'approved') statusLines.push(t('status.approved'))
    if (candidate.status === 'withdrawn') statusLines.push(t('status.withdrawn'))
    if (candidate.status === 'returned') {
      const key = candidate.return_reason !== null && Object.hasOwn(RETURN_KEY, candidate.return_reason)
        ? RETURN_KEY[candidate.return_reason as keyof typeof RETURN_KEY]
        : 'return.other'
      statusLines.push(t('status.returned', { reason: t(key) }))
    }
    if (candidate.note !== null && candidate.status === 'returned') statusLines.push(t('status.note', { note: candidate.note }))
    if (candidate.edited) statusLines.push(t('status.edited'))
    statusLines.push(t('status.checked', { time: when(candidate.checked_at) }))
  }

  const leave = (go: () => void): void => {
    if (dirty && !window.confirm(t('confirm.discard'))) return
    go()
  }

  const onSave = (): void => {
    if (busy || !dirty) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.updateDraft(current.id, { kind, title, body }),
      (value) => applySaved(value, t('save.not_saved')),
      () => setError(t('save.not_saved')),
    )
  }

  const onRedact = (): void => {
    if (busy) return
    // Main redacts what it has stored: edits not saved yet would be lost.
    if (dirty && !window.confirm(t('confirm.discard'))) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.redactDraft(current.id),
      (value) => applySaved(value, t('save.not_saved')),
      () => setError(t('save.not_saved')),
    )
  }

  const applySubmit = (value: unknown): void => {
    if (!isSubmitAnswer(value)) { setError(t('error.failed')); return }
    const next = value.submission
    if (next) setDraft((current) => (current ? { ...current, submission: next } : current))
    if (!value.ok) setError(refusal(t, value))
  }

  const onSubmit = (): void => {
    if (busy) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.submit(current.id, chosenKb),
      applySubmit,
      () => setError(t('error.failed')),
    )
  }

  const onStatus = (): void => {
    if (busy) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.submitStatus(current.id),
      applySubmit,
      () => setError(t('error.failed')),
    )
  }

  const onRetract = (): void => {
    if (busy) return
    if (!window.confirm(t('retract.confirm'))) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.retractSubmission(current.id),
      applySubmit,
      () => setError(t('error.failed')),
    )
  }

  const onWithdraw = (): void => {
    if (busy) return
    if (!window.confirm(t('withdraw.confirm'))) return
    const current = draft
    run(
      () => window.agent24.knowledgeEntry.withdrawDraft(current.id),
      (value) => {
        if (value === true) openDraftList()
        else setError(t('withdraw.failed'))
      },
      () => setError(t('withdraw.failed')),
    )
  }

  return (
    <Shell>
      <label htmlFor="knowledge-entry-draft-kind">{t('field.kind')}</label>
      <select id="knowledge-entry-draft-kind" value={kind} disabled={busy} onChange={(e) => { if (isKind(e.target.value)) setKind(e.target.value) }}>
        <option value="sop">{t('kind.sop')}</option>
        <option value="faq">{t('kind.faq')}</option>
        <option value="note">{t('kind.note')}</option>
      </select>
      <label htmlFor="knowledge-entry-draft-title">{t('field.title')}</label>
      <input id="knowledge-entry-draft-title" type="text" value={title} disabled={busy} onChange={(e) => setTitle(e.target.value)} />
      <label htmlFor="knowledge-entry-draft-body">{t('field.body')}</label>
      <textarea id="knowledge-entry-draft-body" rows={12} value={body} disabled={busy} onChange={(e) => setBody(e.target.value)} />
      <p>{t('detail.source', { from: draft.message_from + 1, to: draft.message_to + 1 })}</p>
      {draft.target === 'changed' && <p>{t('detail.targetChanged')}</p>}
      {draft.target === 'none' && <p>{t('detail.targetNone')}</p>}
      {draft.findings.length > 0 && <p>{t('detail.findings', { categories })}</p>}
      {createdModel && <p>{createdModel}</p>}
      {createdHints && <p>{createdHints}</p>}
      {error && <p role="status">{error}</p>}
      <button className="btn btn-primary" type="button" disabled={busy || !dirty} onClick={onSave}>{t('action.save')}</button>
      {draft.findings.length > 0 && (
        <button className="btn" type="button" disabled={busy} onClick={onRedact}>{t('action.redact')}</button>
      )}
      <button className="btn" type="button" disabled={busy} onClick={onWithdraw}>{t('action.withdraw')}</button>
      {canSubmit && kbs.length > 1 && (
        <>
          <label htmlFor="knowledge-entry-draft-target">{t('submit.target')}</label>
          <select id="knowledge-entry-draft-target" value={chosenKb} disabled={busy} onChange={(e) => setTargetKb(e.target.value)}>
            {kbs.map((kb) => <option key={kb} value={kb}>{kb}</option>)}
          </select>
        </>
      )}
      {canSubmit && (
        <button className="btn" type="button" disabled={busy || blocker !== null} onClick={onSubmit}>{t('action.submit')}</button>
      )}
      {resend && (
        <button className="btn" type="button" disabled={busy || draft.target !== 'same'} onClick={onSubmit}>{t('action.resubmit')}</button>
      )}
      {candidate && (
        <button className="btn" type="button" disabled={busy || draft.target !== 'same'} onClick={onStatus}>{t('action.status')}</button>
      )}
      {candidate?.status === 'submitted' && (
        <button className="btn" type="button" disabled={busy || draft.target !== 'same'} onClick={onRetract}>{t('action.retract')}</button>
      )}
      {canSubmit && blocker && <p>{blocker}</p>}
      {statusLines.map((line, index) => <p key={index}>{line}</p>)}
      <button className="btn" type="button" onClick={() => leave(openDraftList)}>{t('action.back')}</button>
      <button className="btn" type="button" onClick={() => leave(closeDrafts)}>{t('action.close')}</button>
    </Shell>
  )
}

const HiddenContext = createContext(false)

/**
 * The drafts list, or one draft. Renders nothing while the panel is closed.
 * `hidden` keeps it mounted under another panel, so unsaved edits stay.
 */
export function KnowledgeEntryDraftsPanel({ hidden = false }: { hidden?: boolean }): JSX.Element | null {
  const state = useDraftsPanel()
  if (!state) return null
  return (
    <HiddenContext.Provider value={hidden}>
      {/* A new detail per draft: the panel stays mounted now, and nothing of
          one draft (fields, busy, a reply still on its way) may carry over to
          the next. */}
      {state.view === 'list' ? <DraftList /> : <DraftDetail key={state.id} id={state.id} created={state.created} />}
    </HiddenContext.Provider>
  )
}

/**
 * One line on the Knowledge page. Asks for the list on mount and whenever
 * this panel closes. Nothing to show when there are no drafts, or no bridge.
 */
export function KnowledgeEntryDraftsBar(): JSX.Element | null {
  const t = useT(draftMessages)
  const open = useDraftsPanel() !== null
  const [count, setCount] = useState<number | null>(null)
  useEffect(() => {
    if (open) return
    let live = true
    const list = window.agent24?.knowledgeEntry?.listDrafts
    if (typeof list !== 'function') {
      setCount(0)
      return
    }
    invoke(() => list()).then(
      (value) => { if (live) setCount(Array.isArray(value) ? value.filter(isSummary).length : 0) },
      () => { if (live) setCount(0) },
    )
    return () => { live = false }
  }, [open])
  if (open || !count) return null
  return (
    <p>
      {t('bar.count', { n: count })}
      {' '}
      <button className="btn" type="button" onClick={() => openDraftList()}>{t('bar.view')}</button>
    </p>
  )
}
