# Intent: compare what steps and notifications say, in `diff-query`

A step of a flow names its tainted vertex and callee only in message text, which `diff-query`
does not compare. `diff-query` compares them as structured step properties, `taintVertex` and
`callee`: on every step by default, and on the first and last step under `--endpoints-only`,
so a path's source and sink are compared down to the argument.

`ctadl query` does not write these properties yet; that is `intent2.md`, for after this lands.
Until then a step has neither, and absent compares equal to absent.

Also compare `invocation.toolExecutionNotifications`, but not their message text.

Also compare the invocation's `executionSuccessful` and `exitCode`.
