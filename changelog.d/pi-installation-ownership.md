- Fix Pi setup and status refusing a machine-local SDK or Node installation beneath the invocation
  directory when it is outside any Git repository. Repository-owned installations remain refused
  before execution. Repository discovery uses an admitted system Git, never an executable from
  PATH, and runs once per setup or probe operation.
