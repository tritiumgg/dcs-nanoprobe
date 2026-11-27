export const meta = {
  name: 'review-round',
  description: 'One adversarial review round, or the final judge, for a nanoprobe step',
  phases: [
    { title: 'Review', detail: '3 adversarial reviewers', model: 'opus' },
    { title: 'Dedupe', detail: 'merge duplicate issues', model: 'sonnet' },
    { title: 'Vote', detail: '3 voters', model: 'haiku' },
    { title: 'Judge', detail: 'complete and mergeable?', model: 'opus' },
  ],
}

// args:
//   mode      'round' (default) or 'judge'
//   root      the repository's absolute path
//   step      what the work is, e.g. 'S6 Transport and inspector'
//   work      what to review: the files, how to see the diff, the exit test, what is out of scope
//   context   documents the work must satisfy: plan sections, specs, results (absolute paths)
//   lenses    optional [{key, prompt}], three of them; the defaults are below
//   round, of the round's number and the planned total, for the reviewers
//   fixed     issues fixed after earlier rounds, as short lines
//   rejected  issues rejected by vote in earlier rounds, as short lines
//   intended  known and intended differences that are not to be reported
//   history   judge mode: one line per round, e.g. 'round 1: 12 issues, 2 High, all fixed'
// fixed, rejected, intended and history each take a list of strings; a string is a one-item list,
// and a missing value or an empty string is none. Any other type stops the run.

const a = args || {}
const mode = a.mode || 'round'
const list = (title, value) => {
  if (value === undefined || value === null || value === '') return ''
  const items = typeof value === 'string' ? [value] : value
  if (!Array.isArray(items)) throw new Error(`review-round: ${title} expects a string or a list of strings`)
  return items.length ? `\n${title}\n${items.map(x => '- ' + x).join('\n')}\n` : ''
}
const BRIEF = `Step under review: ${a.step}. Repository: ${a.root}.
${a.work}
What the work must satisfy:
${a.context}
You are read-only: do not edit, create or delete any file, and do not run a git command that changes state.${list('Known and intended differences. Do not report these:', a.intended)}`

if (mode === 'judge') {
  phase('Judge')
  const verdict = await agent(
    `You are the final judge. Decide whether the work can be considered complete and mergeable. ${BRIEF}${list('Review history:', a.history)}${list('Fixed after the last round and not yet re-reviewed. Check these:', a.fixed)}
Do this:
1. Run \`mise run check\` and report whether it passes. Run any other check the step's exit test names that runs on the host.
2. Read all of the work and the documents it must satisfy.
3. Judge against the step's exit test and this bar: a remaining defect is blocking only if it would lose or duplicate data, crash DCS, contradict a measured result, breach CLAUDE.md, fail the exit test, or make a later step build on something wrong. A gap that a named later step settles is acceptable.
4. Check CLAUDE.md: nothing setup-specific, no secrets, DCS claims tagged, documents in the current state.
Be concrete and brief.`,
    { label: 'judge', phase: 'Judge', model: 'opus', schema: {
      type: 'object',
      properties: {
        mergeable: { type: 'boolean' },
        check: { type: 'string', description: 'the result of mise run check' },
        blocking: { type: 'array', items: { type: 'object', properties: {
          file: { type: 'string' }, where: { type: 'string' }, problem: { type: 'string' }, fix: { type: 'string' } },
          required: ['file', 'where', 'problem', 'fix'] } },
        remarks: { type: 'array', description: 'at most ten non-blocking remarks, most important first', items: { type: 'string' } },
      },
      required: ['mergeable', 'check', 'blocking', 'remarks'],
    } })
  return verdict
}

const LENSES = a.lenses || [
  { key: 'conformance', prompt: 'Lens: conformance. Check the work against every document it must satisfy: the plan sections, the specs in spec/ (and spec/transport.golden for anything on the wire), the measured results, and the project rules in CLAUDE.md. Report what the work omits, contradicts or weakens, and what it invents against them.' },
  { key: 'correctness', prompt: 'Lens: correctness. Hunt for bugs: wrong logic, races, crash and restart cases, unchecked input from the other process, undefined behavior in unsafe code, a Lua error that could unwind through a Rust frame, a panic that could escape an entry point, work on the sim thread that is not under the budget. Read the code and the tests; run the tests if useful.' },
  { key: 'evidence', prompt: 'Lens: tests and evidence. Does each test fail when the thing it checks is broken? Is the exit test met by what is there, or only claimed? Is any result reported without the script, build, platform and date that produced it? Look for missing cases, tests that pass for the wrong reason, and tooling that would accept bad input.' },
]

const ISSUE = {
  title: { type: 'string' },
  severity: { type: 'string', enum: ['Low', 'Medium', 'High'] },
  file: { type: 'string' },
  where: { type: 'string', description: 'section, function or line' },
  problem: { type: 'string', description: 'what is wrong, with the evidence' },
  fix: { type: 'string', description: 'the concrete change proposed' },
}
const ISSUE_KEYS = ['title', 'severity', 'file', 'where', 'problem', 'fix']
const round = a.round ? `Round ${a.round}${a.of ? ' of ' + a.of : ''}. ` : ''

phase('Review')
const reviews = await parallel(LENSES.map(l => () => agent(
  `You are an adversarial reviewer. ${round}Your job is to find real problems, not to praise. ${BRIEF}
${l.prompt}
${list('Fixed after earlier rounds. Verify the fixes hold, and report one that is wrong or incomplete:', a.fixed)}
Give each issue a severity: High (wrong design, lost or duplicated data, a crash, a contradiction of the plan, a spec or a measured result, or something that blocks a later step), Medium (a gap or ambiguity an implementer would trip on, or an inconsistency), Low (wording, style, minor omissions). Name the file and the place, quote the evidence, and propose a concrete fix. Do not raise a design preference, or work that belongs to a later step. Do not pad: an empty list is a valid answer.`,
  { label: `review:${l.key}`, phase: 'Review', model: 'opus', schema: {
    type: 'object', properties: { issues: { type: 'array', items: { type: 'object', properties: ISSUE, required: ISSUE_KEYS } } }, required: ['issues'] } })))
const raw = reviews.filter(Boolean).flatMap((r, i) => r.issues.map(x => ({ ...x, lens: LENSES[i].key })))
log(`${raw.length} raw issues`)
if (!raw.length) return { raw: 0, accepted: [], rejected: [] }

phase('Dedupe')
const deduped = await agent(
  `Deduplicate this list of review issues. Merge issues that describe the same underlying problem into one: keep the highest severity, the clearest problem statement and the most concrete fix, and name both places if they differ. Do not drop, soften or invent issues, and do not judge whether an issue is right. Number the result from 1.${list('Rejected by vote in earlier rounds. Drop an issue that only repeats one of these:', a.rejected)}
Issues:
${JSON.stringify(raw, null, 1)}`,
  { label: 'dedupe', phase: 'Dedupe', model: 'sonnet', schema: {
    type: 'object', properties: { issues: { type: 'array', items: { type: 'object',
      properties: { id: { type: 'integer' }, ...ISSUE }, required: ['id', ...ISSUE_KEYS] } } }, required: ['issues'] } })
const issues = deduped ? deduped.issues : raw.map((x, i) => ({ id: i + 1, ...x }))
log(`${issues.length} issues after dedupe`)

phase('Vote')
const ballots = await parallel([1, 2, 3].map(i => () => agent(
  `You are voter ${i} of 3. For each issue below, decide whether it is legitimate and should be fixed before this work is committed. ${BRIEF}
For every issue, open the file it names and check the claim against the text or code. Check the plan, spec or results document when the issue cites one. Vote fix=true only if you confirmed the problem is real as stated and a fix belongs to this step. Vote fix=false if the claim is wrong, already handled, out of scope for this step, a matter of taste, or if you could not confirm it. Say in the reason what you checked. Vote on every id.
Issues:
${JSON.stringify(issues, null, 1)}`,
  { label: `vote:${i}`, phase: 'Vote', model: 'haiku', schema: {
    type: 'object', properties: { votes: { type: 'array', items: { type: 'object',
      properties: { id: { type: 'integer' }, fix: { type: 'boolean' }, reason: { type: 'string' } },
      required: ['id', 'fix', 'reason'] } } }, required: ['votes'] } })))
const tally = issues.map(issue => {
  const votes = ballots.filter(Boolean).map(b => b.votes.find(v => v.id === issue.id)).filter(Boolean)
  return { ...issue, yes: votes.filter(v => v.fix).length, voters: votes.length,
    reasons: votes.map(v => (v.fix ? 'FIX: ' : 'NO: ') + v.reason) }
})
const accepted = tally.filter(t => t.yes >= 2)
return {
  raw: raw.length,
  high: accepted.filter(t => t.severity === 'High').length,
  accepted,
  rejected: tally.filter(t => t.yes < 2).map(t => ({ id: t.id, title: t.title, severity: t.severity, yes: t.yes, reasons: t.reasons })),
}
