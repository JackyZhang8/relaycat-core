# Remove pairing expiry

The user approved removing the 24-hour pairing lifetime because GUI tasks can run for many days. Pairing credentials and QR images remain usable regardless of age. New authenticated app identities can join a running session at any time. Proof verification, public-key validation, connection salts, fresh rooms for new runs, and explicit revocation remain in place.

Implementation plan:
1. Reproduce rejection of a new app after the deadline, replacement of old cached credentials, and deletion of old QR images with regression tests.
2. Remove admission deadlines, credential expiry checks, age-based QR deletion, and GUI automatic expiry pruning. Keep the stored creation timestamp for display and file compatibility.
3. Update release guidance and verify CLI tests, GUI TypeScript build, and Rust compilation where available.

The already running GUI binary will retain its old in-memory admission deadline until that relay session is restarted; editing stored timestamps cannot update the live handshake.

Additional reviewed fixes (authorized by the user):
- Scope workspace Shell control and output errors to that stream so the main terminal relay remains alive.
- Dispatch Cancel before requesting an ordinary workspace concurrency permit.
- On iOS and Android, update the sync deadline on successful negotiation and terminal application, never on generic peer traffic. Keep Android's watchdog alive in the live phase and restart the deadline when entering recovery.

Validation for these additional fixes: static review and whitespace/syntax checks only. The user explicitly requested no further test runs before committing.
