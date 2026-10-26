# Muse Code fixtures

Copied read-only from the shipped Pi contract at
[588be884153122145435e5b05190815bfa54d378](https://github.com/hemsoft-dev/hs-pi-dashboard/tree/588be884153122145435e5b05190815bfa54d378/internal/usage/testdata/muse),
for Muse Code 1.4.13-R6996.1 / schema 1.

`measured.json` is the sanitized 15% five-hour / 5% weekly reading supplied by the
fleet owner. Every other JSON file is synthetic. The fake identity text in the
null fixture tests that free-form reason/hint never reach UI, persistence or logs.
No fixture demonstrates live account compatibility or a reason-specific auth,
PAYG, entitlement or rate-limit diagnosis. See [the field map and limitations](../../../../docs/MUSE_CODE.md).
