# Good and Bad Tests

## Good Tests

**Integration-style**: Test through real interfaces, not mocks of internal parts.

```typescript
// GOOD: Tests observable behavior
test("user can checkout with valid cart", async () => {
  const cart = createCart();
  cart.add(product);
  const result = await checkout(cart, paymentMethod);
  expect(result.status).toBe("confirmed");
});
```

Characteristics:

- Tests behavior users/callers care about
- Uses public API only
- Survives internal refactors
- Describes WHAT, not HOW
- One behaviour per test, with as many assertions as that behaviour needs

## Bad Tests

**Implementation-detail tests**: Coupled to internal structure.

```typescript
// BAD: Tests implementation details
test("checkout calls paymentService.process", async () => {
  const mockPayment = jest.mock(paymentService);
  await checkout(cart, payment);
  expect(mockPayment.process).toHaveBeenCalledWith(cart.total);
});
```

Red flags:

- Mocking internal collaborators
- Testing private methods
- Asserting on call counts/order
- Test breaks when refactoring without behavior change
- Test name describes HOW not WHAT
- Verifying through external means instead of interface
- Reads a `.md` file and asserts a sentence, a phrase or a phrase count
- Asserts a constant equals its own literal instead of the behaviour that
  depends on it
- Would still pass if the feature were deleted and only the document remained
- Pins a class name, inline style, DOM path or element index
- Pins a count, order or list that is not itself a contract, or a list that
  must mirror another list
- Reads a source file (`.ts`, `.css`, `.html`) as text and matches it
- Pins exact error or log wording instead of an error class or code
- Repeats a case an existing test already covers

```typescript
// BAD: Bypasses interface to verify
test("createUser saves to database", async () => {
  await createUser({ name: "Alice" });
  const row = await db.query("SELECT * FROM users WHERE name = ?", ["Alice"]);
  expect(row).toBeDefined();
});

// GOOD: Verifies through interface
test("createUser makes user retrievable", async () => {
  const user = await createUser({ name: "Alice" });
  const retrieved = await getUser(user.id);
  expect(retrieved.name).toBe("Alice");
});
```

## What a test may pin

A test pins what a user, a caller or an invariant depends on. Every incidental
detail it pins turns a harmless change into a test rewrite.

- **Pin:** observable behaviour; public contracts, wire formats and persisted
  records; money, security, privacy and data invariants; accessible roles and
  names; error classes and stable codes.
- **Do not pin:** exact copy or prose (assert the role, key or one fragment
  that carries meaning); class names, styles or DOM structure; counts of items,
  tokens, files or tools; ordering that is not a contract; a list that mirrors
  another list or inventory; exact error or log wording; size or line
  ceilings; source text; whole-output snapshots; a mock's calls as the only
  assertion.

Before writing a test, name the defect it catches: a plausible mistake in the
production code that would make it fail. If no plausible mistake would, or an
existing test already fails on it, write none. Mutation testing measures this
where a project runs it; a score is a guide, not a target.

## Choosing the form

- **Examples** for named scenarios, boundaries and bug regressions.
- **Properties** for an invariant over many inputs, such as totals conserving,
  a round trip, or a denied role staying denied. Use the project's
  property-testing library, keep the failing seed, and add the shrunk
  counterexample as an example test when it finds a bug.
- **UI:** query by accessible role and name; a test id is the last resort.
  Assert what the user sees and can do. Appearance, layout and motion need a
  rendered capture or visual diff, not a DOM unit test. A snapshot stays small,
  focused and reviewed.
- **Errors:** assert the class, code or structured fields; assert the message
  only when that text is a public contract.
- **Time and randomness:** inject a clock or use fake timers, and seed
  randomness, so a run is reproducible.
- **Layer:** test each behaviour at the lowest layer that owns it, then enough
  integration to prove the real boundaries join. Neither the pyramid nor the
  trophy is a quota.

## Not observable behaviour

Prose is not a contract. Documentation, `SKILL.md` and reference wording, ADR
and specification text, commit and PR bodies, and the literal values of
configuration keys are not behaviour and earn no test. A test that greps a
document, asserts an exact sentence or counts occurrences of a phrase can only
fail when a maintainer rewords something deliberately: it fires on every
intentional edit and sleeps through every defect.

Before writing any test, name the production change that would make it fail. If
the only answer is "someone changed the text", there is no test to write.

Where a document carries a machine-checkable invariant (a route in a fixture, a
JSON policy value, a path that must resolve, a schema field) test the parsed
structure, or run the script and assert its output, exit code or side effect.
Never assert the source text itself.

## Where a test lives

Before writing a test, find how this repository already tests the unit under
change: its layout, naming convention, focused-test command and full-suite
command. Put the new test in the existing file that owns that unit's behaviour.

A new test file is justified only when a new unit of production behaviour has no
owning file: a new module, script or entry point. A new file is never justified
by a review, an audit finding, an issue number or a documentation pass. Those
add cases to the owning file, or add nothing. Name test files after the
behaviour they protect, never after the process that prompted them. One test
covers one behaviour; if its name joins two behaviours with "and", split it.
Several assertions that together establish one behaviour stay in one test.
Prefer plain, explicit test code over clever helpers: a reader should see the
behaviour without tracing indirection.

## Sources

Kent Beck, [Test Desiderata](https://medium.com/@kentbeck_7670/test-desiderata-94150638a4b3);
*Software Engineering at Google*, [ch. 12](https://abseil.io/resources/swe-book/html/ch12.html)
and [ch. 13](https://abseil.io/resources/swe-book/html/ch13.html);
[Change-detector tests considered harmful](https://testing.googleblog.com/2015/01/testing-on-toilet-change-detector-tests.html);
Vladimir Khorikov, *Unit Testing Principles, Practices, and Patterns*;
[Testing Library guiding principles](https://testing-library.com/docs/guiding-principles/);
[Practical mutation testing at scale](https://research.google/pubs/practical-mutation-testing-at-scale-a-view-from-google/).
