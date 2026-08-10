---
version: 1.0.0
source: https://codeberg.org/SakuraIndustries/FeelsVer.git
license: https://creativecommons.org/licenses/by-sa/4.0/
---

# FEELSVER

## The Release Strategy for Pragmatic Engineers

Three-tier version numbering (`MAJOR.MINOR.PATCH`) predates SemVer by at least 30 years.
SemVer did not invent this shape, it attached a specific, narrow contract to it.

FeelsVer uses the same version shape that the software industry has always used to communicate release magnitude,
and that tooling already knows how to parse.

This project explicitly rejects **Semantic Versioning (SemVer)**.
We do not use it, we do not believe in it, and we treat it as an industry farce.

Instead, this repository utilizes **FeelsVer** (Intuitive Versioning).
We increment version numbers based on engineering intuition, judgement, internal roadmap milestones, and collective code density.

---

## Why We Reject SemVer

SemVer presents a false promise: that software stability can be mathematically defined by a three-tiered version string
(`MAJOR.MINOR.PATCH`).

This is a technical impossibility for several fundamental reasons:

1. **Hyrum’s Law is Absolute:** An API is not just its public function signatures.
   If your code modifies an internal execution timing, fixes an undocumented bug that a downstream user was relying on,
   or updates a diagnostic error string, you have introduced a breaking change.
   It is mathematically impossible to track every infinite permutation of downstream environments.
2. **The "API vs. Toolchain" Lie:** Maintainers regularly push `0.0.1` patches that leave the public API untouched but drop support
   for a compiler version or upgrade a build-tool dependency.
   The API didn't change, but your CI pipeline is broken.
   SemVer ignores the entire developer environment.
3. **The Lifecycle Trap:** SemVer forces software into an artificial binary state:
   chaotic instability (`v0.x`) or complete architectural calcification (`v1.x`).
   Moving from `v1` to `v2` requires a massive,
   high-risk "cold drop" that splits communities and introduces an ongoing dual-maintenance tax on creators.
   Or alternatively people work AROUND this by creating fake versions like v1.99.xxx to signal
   (we are building V2 up here) which itself is NOT SemVer.
4. **It is only for libraries:** SemVer defines a "public API" and says the version number describes changes to that API.
   Nothing else.
   Not applications with a frontend.
   Not tools with a CLI.
   Not database schemas, config formats, or CI environments.

   If your application has a REST API and a UI, and the API is completely unchanged but the UI was redesigned from scratch,
   SemVer says that is a PATCH release.
   The version number communicates nothing about the scale of change your users will actually experience.
   If your API is stable but you bumped the minimum Rust toolchain by two years and now half your users' CI pipelines need work,
   that is also a PATCH.

   And if your CLI happens to be used in scripts — is that an API?
   Nobody agrees, and the answers are always domain-specific post-hoc justifications
   for whatever bump the maintainer already wanted to make.

   FeelsVer does not have this problem because the version number describes the *release*, not one arbitrarily privileged surface.
   If the UI was remade, that release is big.
   If the toolchain bumped, that release carries structural weight.
   The maintainer makes a holistic judgment.
5. **Public does not mean used:** An API can break a public function that 0.01% of users touch, and SemVer demands a MAJOR bump.
   If you bump MAJOR, 99.99% of users see a compatibility warning for something that does not affect them.
   If you bump PATCH, the 0.01% get broken without warning.
   There is no mechanically correct answer.

   Many APIs also expose functions that are technically public but exist for internal plumbing — hazmat crypto primitives,
   debug hooks, unstable utility functions.
   Changing them *is* a breaking change on paper.
   Almost nobody should have been using them.
   The maintainer is forced into a false choice: signal danger that doesn't exist, or break a social contract.

   FeelsVer sidesteps this entirely.
   The maintainer judges the practical impact, not the technical classification.

We take the view that breakage is breakage regardless of WHY it occurs.
SemVer is a social contract masquerading as mathematics.
Because it relies entirely on human interpretation, everyone is already practicing intuitive versioning,
they just use corporate jargon to hide it.
We prefer honesty.

---

## How FeelsVer Works

Our version increments reflect the **architectural weight** of a release as evaluated by the project founders.

We do not go out of our way to break downstream systems without a clear, valid reason; however,
we refuse to let the codebase lock us into permanent stagnation either.

We use the same version string shape as SemVer (`MAJOR.MINOR.PATCH`) for tooling compatibility.
We do not use the same guarantees.

These fields communicate maintainer judgment about release weight.
They are not compatibility proofs.
A larger number means the maintainers felt the release carried more architectural weight.
It does not mean smaller numbers are safe, and it does not mean larger numbers require every user to change code.

* **`0.0.x` (Localized Checkpoint):** SemVer would call this *PATCH*.
  We feel these changes are localized, low-risk, or incremental.
  They pass our internal container tests.
  You can reasonably expect that the smaller the bump, the less likely it is to cause problems—
  but that is a "feels" statement based on probability, not a mechanical guarantee.
  If you have a large cumulative count in this position you should expect that bump to be less likely to go smoothly.
* **`0.x` (Milestone Checkpoint):** SemVer would call this *MINOR*.
  We feel the codebase has accumulated enough structural refinements, new features, any other criteria,
  including known likely breakage, to warrant a fresh architectural baseline.
  Changes here carry more structural weight.
  They are MORE likely to signal that using this release can cause breakage.
  Breakage is NOT restricted to just API's.
  If a major CLI menu changes, or a UI change is significant,
  or if your CI pipeline is likely to break because we bumped the minimum compiler version.
  ALL of that is a candidate for bumping this, and resetting the patch version.
* **`x.0.0` and Beyond (System Maturity):** SemVer would call this *MAJOR* and leave you no good options to iterate to V2.
  The system feels complete, cohesive, and fulfills its core purpose.
  We stamp it when it feels right, and we iterate from there.
  Eventually we accumulate enough changes, that we can bump to V2 and so on.
  We favor incremental evolution over sudden massive feature complete releases.

**While we respect operational continuity, including our own, we reserve the right to restructure internals, modify CLI outputs,
alter diagnostics, or evolve downstream behavior at any version boundary when engineering needs dictate it.**

---

## FeelsVer and Release Tooling

Automated version-bumping tools (conventional commits, release-please, changelog generators) work better under FeelsVer than under
SemVer, not worse.

Under SemVer, these tools must forensically classify every commit as fix/feat/breaking,
and the maintainer must then audit the tool's suggestions against the "was this *really* an API boundary change?" question.
A commit tagged `feat!` because someone rewrote a CLI flag is technically not an API change at all.
A commit tagged `fix` because someone bumped the minimum compiler version by two years has changed nothing public
but will shatter CI pipelines.

Under FeelsVer, you let the tool suggest a bump based on commit conventions as a rough signal, and then you apply human judgment.
You are not auditing whether each commit technically touched a public API boundary.
You are asking: does this release *feel* like a patch, a milestone, or a new era?
The tooling helps surface the raw data; the maintainer applies the weight.

---

## Mandatory Guidance for Downstream Users

If you are using this project, you are expected to practice adult engineering discipline.
Do not delegate your infrastructure's security or stability to automated bots or semantic wildcards.

1. **PIN Your Dependencies:** Pin your project configuration to the **exact version string**
   or **git commit SHA hash** you have manually verified.
2. **Ban Automated Bumping Tools:** Turn off Dependabot, Renovate,
   and any other automated package managers that nervously bump point releases every 40 seconds.
   These tools act on absolute semantic ambiguity and regularly pull down supply chain compromises
   or broken environments during blind spots.
3. **Update Thoughtfully:** Treat every single update—even a `0.0.1` bump—as an unverified, untrusted third-party code deployment.
4. **Isolate and Verify:** Pull updates on a deliberate schedule (e.g., weekly) into an **isolated build container**.
   Run your own test matrix, verify that it compiles against your toolchain,
   and manually commit the lockfile change only after verification.

If your system shatters because you blindly pulled an update without testing it inside your own sandbox first,
you have violated your own operational hygiene.
You are welcome to fork the project and maintain it on your own terms.
Including reintroducing SemVer should you feel the need.

TLDR; If an update to this package breaks your build you have TWO choices.
Migrate to the new version or pin the old one until you're ready to migrate.
