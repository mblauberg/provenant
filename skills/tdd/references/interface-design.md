# Interface Design for Testability

Good interfaces make testing natural:

1. **Accept dependencies, don't create them**

   ```rust
   // Testable
   fn process_order(order: &Order, gateway: &impl PaymentGateway) -> Result<Receipt, Error> {
       gateway.charge(order.total_cents)
   }

   // Hard to test
   fn process_order_from_env(order: &Order) -> Result<Receipt, Error> {
       let gateway = StripeGateway::from_env()?;
       gateway.charge(order.total_cents)
   }
   ```

2. **Return results, don't produce side effects**

   ```rust
   // Testable
   fn calculate_discount(cart: &Cart) -> Discount {
       Discount::in_cents(cart.subtotal_cents() / 10)
   }

   // Hard to test
   fn apply_discount(cart: &mut Cart) {
       cart.total_cents -= cart.subtotal_cents() / 10;
   }
   ```

3. **Small surface area**
   - Fewer methods = fewer tests needed
   - Fewer params = simpler test setup
