# ADR-0059: Account Import reuses deleted Usernames

- Status: Accepted
- Date: 2026-08-02
- Deciders: shop owner (product)
- Refines: [ADR-0011](0011-admin-authority.md), [ADR-0017](0017-username-identity.md), [ADR-0057](0057-account-import-from-legacy-systems.md)

A later **Account Import** may reuse the Username of any deleted Account by creating a new Account identity. It never restores or overwrites the deleted Account; that identity and its financial/audit history remain preserved. Active Username conflicts remain rejected, and manual Account creation and self-service signup cannot reuse deleted Usernames. This permits clean re-imports without destroying canonical records or conflating old and new balances/history.
