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
- One logical assertion per test

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
asserts one behaviour; if the name needs "and", split it.
