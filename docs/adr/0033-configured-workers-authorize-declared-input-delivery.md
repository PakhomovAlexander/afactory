# Treat configured Workers as authorization for declared input delivery

**Status:** accepted (2026-08-28)

A trusted Afactory pipeline names each Worker, its Provider binding, its exact input ports, its
package, and its budgets. The operator then intentionally starts a review Campaign or
implementation Task under that authority. Asking again before every Provider call or later Round
adds an interactive control outside the recorded policy, prevents unattended bounded execution,
and makes agents mistake ordinary Campaign continuation for a new external action.

Trusting the configured authority and intentionally invoking `af review run` or `af task start`
authorizes Afactory to deliver each configured Worker exactly the inputs declared for it. That
authorization covers retries and later Rounds or stages of the same Campaign or Task. Afactory and
agents operating it do not ask for separate per-Worker, per-Attempt, or per-Round confirmation.

The authorization is bounded by the trusted Authority Snapshot, Worker and Provider binding,
typed input graph, context limits, budgets, and sandbox policy. It does not authorize undeclared
repository data, ambient parent transcripts, another Worker's private reasoning, a changed
Provider binding, hosted execution, credential mutation, delivery, publication, push, pull
request, comment, or other remote side effect. Those remain separate explicit operations.

## Considered options

- **Ask before every Provider invocation.** Rejected because the same already-authorized data flow
  repeats across retries and Rounds, while interactive prompts break deterministic unattended
  Campaign execution.
- **Treat configuring a Worker alone as authority to run it.** Rejected because onboarding and
  authority inspection must stay deterministic and token-free; an intentional review or Task
  command remains the execution boundary.
- **Treat trusted configuration plus intentional execution as authorization (chosen).** The
  recorded graph says who receives what, and the command says when to execute it.
- **Allow configured Workers to retrieve arbitrary repository or host context.** Rejected because
  consent to the declared input graph is not consent to unbounded disclosure.

## Consequences

- Agents may run and continue configured Afactory Campaigns and Tasks without asking the user to
  reconfirm each model call.
- Changing authority or Provider bindings remains visible, reviewed project policy; starting work
  under the changed authority is a new intentional execution.
- Provider authentication remains machine-local and may independently fail, but authentication
  status is not a per-call consent mechanism.
- A host agent platform may impose a stronger external-egress policy that Afactory cannot bypass;
  such a prompt is imposed by that host, not by the Afactory contract.
