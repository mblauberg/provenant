# Requirements and Planning Documents

Use this reference for the formal deliverables produced when scoping and starting a project: requirements specifications, user stories, scope statements, stakeholder analysis, business cases, software development plans, roadmaps, and estimates. Each entry gives the reader's job, a skeleton, and the discipline that keeps the document honest. Choose the shape from the reader's decision, not from the material you happen to have.

## Contents

- [Requirements specification (SRS)](#requirements-specification-srs)
- [User stories and acceptance criteria](#user-stories-and-acceptance-criteria)
- [Scope statement and statement of work](#scope-statement-and-statement-of-work)
- [Stakeholder analysis](#stakeholder-analysis)
- [Business case](#business-case)
- [Software development plan](#software-development-plan)
- [Roadmap](#roadmap)
- [Estimation and costing](#estimation-and-costing)

## Requirements specification (SRS)

Reader's job: know exactly what the system must do and how well, precisely enough to build, test, and accept it. Grounded in ISO/IEC/IEEE 29148.

Skeleton:

1. Purpose, scope, and definitions (what system, what boundary, glossary).
2. Overall description: context, users, assumptions, dependencies, constraints.
3. Functional requirements: what the system does, grouped by feature or capability.
4. Non-functional / quality requirements: how well it does it.
5. External interfaces: users, hardware, software, communications.
6. Verification: how each requirement will be confirmed.

Separate functional from non-functional (quality) requirements:

- **Functional**: an action the system performs. `The system shall lock an account after 5 consecutive failed sign-in attempts.`
- **Non-functional / quality attribute**: a property the system must hold, drawn from ISO/IEC 25010 attributes (performance efficiency, reliability, security, usability, maintainability, portability, compatibility). `95% of search requests shall return within 200 ms at 100 concurrent users.`

Write each requirement so it is, per 29148, *necessary, singular, unambiguous, complete, feasible, verifiable, correct, conforming, and appropriate*. In practice:

- One requirement per statement (singular). Split compound `and`/`or` requirements.
- Verifiable: state a measurable condition. `Fast` and `user-friendly` are not requirements; `returns within 200 ms` and `a new user completes checkout without help in under 3 minutes` are.
- Use a consistent modal: `shall` for a binding requirement, `should` for a goal, `may` for an option. Do not mix `shall`/`will`/`must` at random.
- Give every requirement a stable identifier (`FR-012`, `NFR-004`) for traceability to tests and design.
- State assumptions and dependencies explicitly; an unstated assumption is a defect.
- Avoid design in a requirement. State the need (`the system shall notify the user of a failed payment`), not the implementation (`the system shall send an email via SendGrid`), unless the mechanism is itself a constraint.

Bad: `The system should be fast and handle lots of users securely.`
Good: `NFR-007 (performance): the product-search endpoint shall return the first result page within 200 ms (p95) at 100 concurrent users. NFR-008 (security): all traffic shall use TLS 1.2 or higher.`

## User stories and acceptance criteria

Reader's job: understand a unit of user-facing value small enough to build in an iteration, and know when it is done. The agile counterpart to a requirement, not a replacement for a spec.

Format: `As a <role>, I want <capability> so that <benefit>.` The `so that` is not optional; it carries the reason and lets you cut scope without losing intent.

Apply INVEST (Bill Wake):

- **Independent**: buildable without waiting on another story where possible.
- **Negotiable**: a placeholder for a conversation, not a contract of tasks.
- **Valuable**: delivers something a user or customer can perceive.
- **Estimable**: the team can size it; if not, it needs more detail or a spike.
- **Small**: fits comfortably in one iteration. If it needs 4+ acceptance criteria, it is probably too big; split it.
- **Testable**: you can write acceptance criteria that pass or fail.

Acceptance criteria define done. Prefer Given/When/Then (Gherkin) for behavioural criteria:

```
Given a signed-in user with an expired session token
When they request a protected resource
Then the API returns 401
And the client redirects to the sign-in page
```

Rules: focus on observable behaviour and outcomes, not implementation. Keep 1 to 3 scenarios per story. Quantify anything vague (`results within 200 ms`, not `quickly`). Write criteria collaboratively before development, not after. Cover the happy path plus the important edge and error cases.

## Scope statement and statement of work

Reader's job: agree on the project boundary so both sides know what is included, excluded, and what "finished" means. A scope statement is internal; a statement of work (SOW) is the client-facing, often contractual, version.

Skeleton:

1. Background and objective (why, in one paragraph).
2. In scope: the deliverables and capabilities included, as a concrete list.
3. Out of scope: what is explicitly excluded. This section prevents most disputes; write it deliberately.
4. Deliverables: each tangible output, with its acceptance definition.
5. Assumptions and dependencies (environments, third parties, client-provided inputs).
6. Constraints (budget, deadline, technology, compliance).
7. Milestones and acceptance: the objective checks and tier-required review
   for each deliverable, including any project landing gate. Explicit user
   acceptance applies only to `terminal` work or when the approved scope asks
   for it.
8. (SOW only) commercial terms, timeline, and change-control process.

Discipline: the out-of-scope list is as important as the in-scope list. State acceptance in observable terms. Name a change-control path so new requests do not silently expand scope. Do not restate the full requirements here; reference the SRS or backlog.

## Stakeholder analysis

Reader's job: know who is affected by or can affect the project, how much each matters, and how to communicate with each. Grounded in ISO/IEC/IEEE 42010 stakeholder identification and the power/interest grid.

Steps:

1. **Identify.** Consider users, operators, acquirers/sponsors, owners, developers, maintainers, suppliers, regulators, and anyone who bears risk or benefit. List individuals or roles, not vague groups.
2. **Assess.** Place each on a power/interest grid (their influence over the project against their concern with it), giving four groups: high power + high interest (manage closely / regularly engage), high power + low interest (keep satisfied), low power + high interest (keep informed), low power + low interest (monitor).
3. **Clarify roles** with a RACI matrix per major decision or deliverable: Responsible (does the work), Accountable (owns the outcome, one person), Consulted (input sought before deciding), Informed (told after).
4. **Plan communication.** For each stakeholder or group: what they need, how often, in what form, and who owns the contact. Tailor depth to the grid position; do not send a high-power sponsor the same detail as a delivery engineer.

Present as a table (stakeholder, role/interest, power, interest, RACI, comms cadence). Record each stakeholder's actual concerns; those concerns drive which requirements and architecture views matter.

## Business case

Reader's job: decide whether to fund the work, by weighing the options against cost and benefit. The audience is usually a sponsor or executive, often non-technical.

Skeleton:

1. Executive summary: the recommendation and the number, in a few sentences a busy reader can act on.
2. Problem or opportunity: what is wrong or possible now, with evidence.
3. Options considered: including "do nothing" as the baseline. For each, a short description, advantages, disadvantages, and estimated cost.
4. Cost-benefit analysis: costs (direct, indirect, ongoing, contingency) against benefits (quantified where possible; state the qualitative ones honestly). Include payback period, ROI, or NPV where the audience expects it.
5. Recommendation: which option and why.
6. Risks and assumptions.
7. High-level timeline and resourcing.

Discipline: always present real options, not one dressed-up choice and two straw men. State assumptions behind every number. Separate one-off from recurring cost. Do not hide the downside of the recommended option; a decision-maker who finds it later distrusts the whole case.

## Software development plan

Reader's job: understand how the project will be run: process, team, schedule, quality approach, and risks. Grounded in the intent of ISO/IEC/IEEE 12207 and IEEE 1058-style planning, sized to the project.

Skeleton:

1. Overview and objectives.
2. Process model and why (see the process choice in `process.md`): iterative/agile, phased, or hybrid, and the cadence.
3. Team, roles, and responsibilities.
4. Work breakdown and schedule / milestones.
5. Engineering practices: version control, branching, code review, CI/CD, testing strategy, definition of done.
6. Quality and verification: what is tested, how, and the acceptance gates.
7. Risk management: top risks, likelihood, impact, mitigation, owner.
8. Configuration and change management.
9. Assumptions, dependencies, and constraints.

Discipline: state the process and justify it against this project's uncertainty and team, rather than naming a methodology as decoration. Keep the plan short enough to stay current; a plan no one updates is worse than a lean one that is trusted.

## Roadmap

Reader's job: see the direction and sequence of work without reading a false promise of exact dates. For a small team facing changing requirements, prefer an outcome-based Now / Next / Later roadmap over a Gantt-style timeline.

- **Now**: in progress or committed, with the most confidence. Update frequently.
- **Next**: likely next, shaped but not started. Update as priorities move.
- **Later**: directional bets, deliberately vague. Update least often.

Anchor each item to an outcome or goal (what changes for users or the business), not a feature name alone. State the theme, the target outcome, and the rough confidence. Reserve dated timelines for genuinely fixed commitments (a launch, a regulatory deadline); mark everything else as horizon-based and note that dates are estimates. A hybrid works when some dates are non-negotiable and the rest are not. Say what the roadmap does not commit to.

## Estimation and costing

Reader's job: understand how long or how much, and how uncertain that figure is. An estimate without its uncertainty is a guess presented as a fact.

- **Respect the cone of uncertainty.** Early estimates carry a wide range (roughly 0.25x to 4x at project start), narrowing as work proceeds. Present early figures as ranges, not single numbers, and re-estimate as uncertainty falls.
- **Three-point / PERT** for individual items: expected = (optimistic + 4 x most likely + pessimistic) / 6. This surfaces risk and builds contingency into the number rather than bolting it on.
- **Relative sizing (story points)** for backlog work, converted to time or cost through observed team velocity, not through a fixed hours-per-point rate assumed up front.
- **Rough order of magnitude (ROM)** for the earliest budget conversation; label it as such so no one treats it as a quote.

Costing document discipline: separate one-off build cost from recurring operating cost (infrastructure, licences, support). State the estimation method and the assumptions. Give a range and name what would move it. Include a contingency line and say what it covers. Never present a single confident number for work that has not been decomposed.
