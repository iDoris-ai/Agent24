# OfficeAssistant — Intern Brief

> Status: planning draft, 2026-10-16. Nothing described here has been built yet.
> Chinese planning documents (for reference; ask if anything is unclear): [`PLAN-OFFICE-ASSISTANT.md`](PLAN-OFFICE-ASSISTANT.md), [`OFFICE-ASSISTANT-TECH-DESIGN.md`](OFFICE-ASSISTANT-TECH-DESIGN.md). Source research (English): [`research/T007-office-assistant-report.md`](research/T007-office-assistant-report.md).

---

## 1. One page: what, M1, your role, your limits

**What we are building.** Agent24 is a local-first AI agent platform. OfficeAssistant is a thin assistant layer inside Agent24's basic office suite. It turns a request ("make minutes and to-dos from this meeting, show me first") into a plan, drafts, actions, and receipts that the user can inspect. Agent24's kernel stays the only place that runs tools, asks for approval, and writes the audit log. We are **not** building a new email client, a Notion replacement, a CRM, or a project-management tool.

**Our promise, made precise.** The user-facing line is: *"Hand off the repetitive work. Before each task is completed, it will wait for your approval."* The testable version is: the assistant prepares results for review first; anything that sends data out, changes something remotely, or widens access only happens under the user's confirmation for that specific action (or a still-valid scope they explicitly granted). Anything outside that scope means asking again.

**Milestone 1 (OA-M1): the local loop.** No real accounts at all. We use synthetic meeting transcripts and a mock connector:

1. Transcript in → meeting minutes + action items out.
2. Every item points to the exact source quote.
3. Missing owner or due date → marked "needs confirmation". **Never invent** commitments that were not said.
4. The user's edits are kept; regenerating must not overwrite them.
5. Local delivery or mock delivery, with a receipt clearly labelled **"simulated"**.
6. Cancel, retry, disconnect and restart all behave safely: nothing is sent twice, and the UI never shows success it cannot prove.

**What you own in M1:**
- Synthetic meeting transcripts and "gold standard" expected outputs (Thai, English, and mixed-language meetings).
- An evaluation script that compares actual vs. expected outputs item by item.
- Negative test cases: rejection, expiry, payload change after approval, cancellation, restart, "instructions hidden inside the transcript".
- The demo script and the acceptance record (pass/fail, evidence, steps to reproduce for each scenario).
- Thai UI copy review.
- Your feedback (Section 3). This matters as much as the code.

**What you must not do (hard rules):**
- No real outbound actions: do not send email, write to Notion, or post on LINE, not even "just to test".
- No credentials: do not create, store, copy, or forward API keys, tokens, OAuth grants, or passwords.
- No production data: no real customer material, and no real meeting recordings or transcripts unless jason provides a de-identified sample in writing.
- No merging, tagging, releasing, or changing branch protection.
- Do not edit `docs/laws/`, ADRs, or kernel approval/audit/authorization code on your own. You may open issues and write tests that touch them.
- Do not run anything on the Mac mini.

**When in doubt: ask first, do not guess.**

---

## 2. Collaboration agreement

| Topic | Agreement |
|---|---|
| Language | English for all written work, PRs, and questions. Thai is welcome inside test data and UI copy. |
| Cadence | One weekly sync (time to be agreed) plus async questions at any time. Expect replies within one working day. Thailand and the rest of the team may be in different time zones, so put the deadline and time zone on any question that is blocking you. |
| Branches | You work on task branches named `ab/office-assistant-NN-<short-name>` (pending approval of the integration branch). Never target `main`. |
| PR size | ≤ 300 changed lines by default. If you can't split it, explain why in the PR body. |
| Before a PR | Run the local checks in `AGENTS.md` (Rust: `cargo fmt --check`, `clippy`, `test`; TypeScript: `pnpm typecheck && pnpm lint && pnpm test` if you touched `apps/`). Then run `pre-pr-check.sh` and answer each finding by number in the PR body. |
| Review | Every PR is reviewed by PR-Daemon (automated reviewer) and a human. Low-risk PRs (test data, scripts, docs) can merge after CI is green and checks are answered. Security, permission, storage-migration, and protocol PRs need an explicit PR-Daemon APPROVE. A maintainer does the merge, not you. |
| Commits | Commit with your own identity. Do not push new commits after a PR has been approved without asking first, because that can invalidate the approval. |
| Test data | Every synthetic file starts with a header line `SYNTHETIC — not real people or companies`. Use made-up names and companies. |
| Asking | If a requirement is ambiguous, write down the two readings you see and ask which one is meant. Do not pick one silently. |

---

## 3. Questions we want your feedback on

Please answer in the template in Section 4. Short answers are fine. "I don't know" is a valid and useful answer.

### (a) Priorities
- **A1.** Here is the M1 feature order: (1) synthetic data + gold standard → (2) extraction with source quotes → (3) "needs confirmation" marking → (4) edit preservation → (5) approval card + mock delivery → (6) cancel/retry/restart → (7) task-detail UI. What would you move earlier or later, and why?
- **A2.** Is there a feature missing from M1 that a Thai office user would expect on day one?
- **A3.** Is anything in M1 not worth doing at this stage?
- **A4.** For the first real connector (M2), the choice is email or Notion. jason decides, but which one would matter more to the people you know, and why?

### (b) Thai workplace and communication habits
- **B1.** What role does LINE play at work in Thailand (team chats, customer contact, approvals, sharing files)? Is it personal LINE, LINE Official Account, LINE WORKS, or a mix?
- **B2.** How are meeting minutes usually written and shared (format, who writes them, how soon after, which channel)? What is a "normal" action item: does it always have an owner and a date?
- **B3.** How are owners and due dates usually expressed in Thai meetings? For example: indirect assignment, honorifics or nicknames (ชื่อเล่น), Buddhist-era years (พ.ศ.), relative dates. Which of these would confuse an extraction model?
- **B4.** When a task is assigned implicitly or politely, should the assistant mark it as "needs confirmation" or treat it as assigned? What would feel respectful rather than awkward?
- **B5.** Mixed-language meetings (Thai/English, or with Chinese speakers): what patterns should our test data include?
- **B6.** Cross-time-zone and cross-language collaboration: what usually goes wrong?
- **B7.** Interface wording: how should "waiting for your confirmation", "result needs verification", and "simulated" read in Thai so that they are clear and polite? Please propose wording.
- **B8.** How do these habits change our feature priorities (for example, should LINE suggested replies come earlier)?

### (c) Process improvements
- **C1.** Is a weekly sync plus async questions the right review rhythm for you? What would work better?
- **C2.** Are English-only documents a barrier? Would a Thai summary of key decisions help?
- **C3.** How would you like acceptance to work: live demo, recorded video, written evidence, or a mix?
- **C4.** How do you prefer to receive feedback on your work (PR comments, a call, a written summary)?
- **C5.** Anything in this brief that is unclear or contradicts itself?

### (d) Pitfalls of building this in Thailand
- **D1.** Legal or compliance concerns you know of (for example PDPA and personal data in meeting transcripts, workplace recording consent).
- **D2.** Language and text-processing pitfalls (Thai has no spaces between words, Thai numerals, date formats, names).
- **D3.** Platform pitfalls (LINE account types, business verification, messaging quotas or costs, workplace tool restrictions).
- **D4.** Cultural pitfalls in automated messages (tone, hierarchy, formality) that could embarrass a user.
- **D5.** Anything else you think we will get wrong.

---

## 4. Feedback template (copy and fill in)

```markdown
# OfficeAssistant feedback — <your name> — <date>

## (a) Priorities
- A1 Reordering (move earlier / later + why):
- A2 Missing day-one feature:
- A3 Not worth doing now:
- A4 Email vs Notion, and why:

## (b) Thai workplace habits
- B1 LINE at work:
- B2 How minutes and action items are usually done:
- B3 How owners and dates are expressed (examples welcome):
- B4 Implicit assignment: confirm or assign?
- B5 Mixed-language patterns for test data:
- B6 Cross-time-zone / cross-language problems:
- B7 Proposed Thai wording:
    - "waiting for your confirmation":
    - "result needs verification":
    - "simulated":
- B8 Impact on priorities:

## (c) Process
- C1 Review rhythm:
- C2 Document language:
- C3 Acceptance format:
- C4 Feedback format:
- C5 Unclear or contradictory points in this brief:

## (d) Pitfalls in Thailand
- D1 Legal / compliance:
- D2 Language / text processing:
- D3 Platform:
- D4 Cultural tone:
- D5 Other:

## Open questions for the team
1.
2.
```

---

## 5. Glossary

| Term | Meaning here |
|---|---|
| Kernel | Agent24's Rust daemon. The only authority for runs, approvals, audit, and permissions. |
| Run | One execution in the kernel. A finished run is never restarted; a retry creates a new run. |
| Action | One step that has an effect (for example "send this email"). Each action has its own approval. |
| Receipt | What actually happened: approved / dispatched / accepted by the provider / verified. These are different things. |
| Mock connector | A fake email/Notion/LINE that never contacts a real service. Its results are always labelled "simulated". |
| LocalOnly | Data that must not leave the machine, including to cloud AI models. Meeting content is LocalOnly by default. |
| Gold standard | The expected correct output for a test input, written by a human. |
