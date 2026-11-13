- Review planning now refuses schema-required Worker inputs that cannot be proved available from
  the captured Task inputs and compiled wiring, before running checks or reviewers. This includes
  Requirements declared through schema composition and local references. Goal-only Reviews whose
  reviewers require Requirements must supply explicit requirements; permissive command Reviews and
  deterministic human-decision Reviews keep their existing contracts.
- The same preflight now checks owned Scatter reviewer templates, their inherited wiring and
  generated Slice inputs. Empty root artifact lists count as absent, and root property counts,
  names and additional-property restrictions participate in the bounded presence proof.
- Schema-valued property restrictions now retain Boolean and reference semantics during preflight.
  Exclusive alternatives require proof that exactly one branch accepts, preventing overlapping
  root-count alternatives from reaching the gate with malformed reviewer inputs.
- Single-branch `oneOf` schemas preserve their branch's input-presence proof, allowing valid
  supplied inputs while leaving value validation to the runtime schema check.
- Required Diff Subject inputs are now proved from the exact declared base or recorded source
  lineage before checks, using the same base resolution as dispatch. Sources without a base or
  with unresolved upstream lineage still fail closed when their reviewer schema requires it.
- The input proof now includes captured knowledge selection and nonempty development cache
  policies, including cold selections. It uses the rendering projection to remove raw queries
  and Notes replaced by knowledge before checking presence and property counts. Effective slot
  packages govern these selections; knowledge-enabled Scatter coordinators fail structural
  admission because that selection route is unsupported.
- Upstream `many` outputs no longer promise a rendered property merely because the output port
  is nonoptional: its future artifact list may still be empty.
- Workers with a knowledge companion now reserve `knowledge_query` for `af/KnowledgeQuery@1`
  on both inputs and outputs. Unrelated types are refused during package admission, before a
  check or reviewer can run, so input projection cannot hide an unrelated declared input.
- Scatter reviewers with nonempty development cache policies are now refused before checks:
  owned child invocations cannot select the promised Development Warm Set on this route.
- Property-name validation preserves the original schema root when resolving local references,
  admitting valid referenced name restrictions while still refusing forbidden input names.
