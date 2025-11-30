CONTRIBUTING
==

Contributions are welcome. Satty is not able to evolve without relying on its contributors and their contributions.

We're always looking for and grateful about help with e.g. documentation/README, PRs, PR reviews, FAQ or other wiki entries.

This documents tries to reduce friction when it comes to contributions by defining some guidelines, some of which may follow a rationale while others are arbitrary determinations.

Please note that opening a PR or even just an issue may expose your work to pertinent discussion regarding code quality, Satty's scope and these guidelines, and possibly things we haven't yet thought of. This isn't meant as discouragement, just as a heads-up.

Issues for bugs or feature requests
--

Bug reports and ideas for Satty are valuable contributions. But please take the time to search for existing issues.

Issue first, then PR
--

The issue should state what is missing from or broken in Satty. All the discussion around whether a feature is in scope, or a behaviour is a bug can take place there. A related PR is then just about correctness of a fix or feature implementation. This ensures that a specific feature or fix is actually wanted.

Commits and PRs
--

- We don't squash the commits in a PR. If you feel that some commits are temporary in nature, please squash them yourself, otherwise they will show up in commit history after we merge the PR.
- If github indicates conflicts, please rebase your branch instead of merging upstream changes. We know that having to rebase sucks, so we're doing our best to point out where conflicts may arise even in advance, but sometimes conflicts are inevitable. We're happy to assist with rebasing, just say the word.
- Please make sure that all commits in a non-draft PR compile, this helps `git bisect`.
- The first commit in a PR and the PR itself should use a conventional commit message. 
- PRs should not break existing config or disrupt existing user workflows. But if there are potential surprises, please add a "!" for attention, e.g. "fix!", "feat!", and provide a small section that may be included in the release notes.

Milestones
--

We use these to indicate which issues and/or PRs we'd ideally like to include with the next release. This doesn't mean any pressure, or that there's any deadline.

3rd party crates
--

We would like to keep 3rd party dependencies to a minimum. Addition of new dependencies should only be considered if
- the relevant code parts are non-trivial
- the functionality in question cannot be provided via existing dependencies

Code comments
--

Ideally, code should be written in a way that it is self-explanatory. Comments can always help make code parts more understandable. They especially make sense when a section
- was tricky to figure out
- is sophisticated or unintuitive or not immediately obvious
- might be in jeooardy of being overwritten by future you or other contributors due to not understanding it properly

Please note that we may ask for additional comments.

Code formatting and hints/improvements
--

Please use `cargo fmt` to apply formatting and `cargo clippy` to fix all suggestions pertinent to your changes. You can use `make fix` for both. Please note that this may apply changes unrelated to your code:
- formatting if previous commits have not used `cargo fmt`
- hints if previous commits have not used `cargo clippy` OR clippy is newer than the last commit and has learned new hints in the meantime

Missing formatting/hints that precede your PR should be addressed via a separate issue/PR in main branch first. If in doubt how to resolve such a situation, ask.

README changes
--

If a PR changes Satty's behaviour and where appropriate, please adjust `README.md` as well. `make update-readme` adds the command line help (output of `satty --help`) automatically which is relevant whenever command line arguments change. While it can be tempting to add other fixes to the README while you're at it, unrelated changes to it which precede your PR should be addressed in a separate issue/PR first. If in doubt how to resolve such a situation, ask.

Command line parameters changes
--

Please include anticipated next version in the comment for command line arguments, especially when adding arguments or options. You can use the placeholder `NEXTRELEASE` in `command_line.rs`, `configuration.rs` and `README.md`.

GenAI usage
--

GenAI usage is tempting and can save time, but it's not without pitfalls. At this point in time, full vibe coding mode can and often does lead to bad quality code which we are not going to merge.

When using GenAI in the context of Satty PRs, please make sure that
- any generated code can actually be licensed under Satty's license, i.e. doesn't violate existing intellectual property
- any generated code actually does what it claims it does
- you have a technical understanding of how the generated code works and you (not the GenAI) can explain it in detail
