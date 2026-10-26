# When to Mock

Mock at **system boundaries** only:

- External APIs (payment, email, etc.)
- Databases (sometimes - prefer test DB)
- Time/randomness
- File system (sometimes)

Don't mock:

- Your own classes/modules
- Internal collaborators
- Anything you control

Prefer a real implementation or an in-memory fake to a mock wherever it runs
locally. Assert the resulting state or return value; assert a mock's calls only
when the call is itself the contract, such as a charge or a sent message.

## Designing for Mockability

At system boundaries, design interfaces that are easy to mock:

**1. Use dependency injection**

Pass external dependencies in rather than creating them internally:

```rust
// Easy to mock
fn process_payment(order: &Order, client: &impl PaymentClient) -> Result<Charge, PaymentError> {
    client.charge(order.total_cents)
}

// Hard to mock
fn process_payment_from_env(order: &Order) -> Result<Charge, PaymentError> {
    let client = StripeClient::from_env()?;
    client.charge(order.total_cents)
}
```

**2. Prefer SDK-style interfaces over generic fetchers**

Create specific functions for each external operation instead of one generic
function with conditional logic:

```rust
// GOOD: Each function is independently mockable
trait ShopApi {
    fn get_user(&self, id: UserId) -> Result<User, ApiError>;
    fn get_orders(&self, user: UserId) -> Result<Vec<Order>, ApiError>;
    fn create_order(&self, order: NewOrder) -> Result<Order, ApiError>;
}

// BAD: Mocking requires conditional logic inside the mock
trait GenericApi {
    fn request(&self, endpoint: &str, body: &[u8]) -> Result<Vec<u8>, ApiError>;
}
```

The SDK approach means:
- Each mock returns one specific shape
- No conditional logic in test setup
- Easier to see which endpoints a test exercises
- Type safety per endpoint
