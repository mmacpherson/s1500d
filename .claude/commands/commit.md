Stage and commit all current changes with a well-crafted commit message.

Follow these steps:

1. Run `git status` (never use `-uall`) and `git diff` (staged + unstaged) and `git log --oneline -10` in parallel to understand the current state and recent commit style.

2. Review the changes and draft a commit message:
   - Summarize the nature of the changes (new feature, enhancement, bug fix, refactoring, etc.)
   - Use imperative mood in the subject line (e.g., "add", "fix", "update")
   - Keep the subject line under 72 characters
   - Add a body with bullet points if multiple logical changes are present
   - Match the style of recent commits in the repo
   - Do NOT commit files that likely contain secrets (.env, credentials, etc.) — warn the user if found

3. Stage the relevant files by name (prefer specific files over `git add -A`).

4. Create the commit using a HEREDOC for the message, ending with the correct co-author for your model:
   - If you are Opus: `Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>`
   - If you are Sonnet: `Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>`

5. Run `git status` after the commit to verify success.

If there are no changes to commit, say so and stop.
