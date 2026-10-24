Repair trusted local telemetry execution under a Linux ancestor subreaper. Keep the script
leader unreaped while ending its owned process group, distinguish terminal processes from
executing survivors with complete bounded evidence, including a zombie leader with live
threads, and keep failed observations uncertain.
Leftover live descendants still fail the run, deadlines still stop the group, and cancellation
or active disk-check errors clean up before propagating. Isolated subreaper regressions reproduce
both predecessor failures without changing the verdict sample bound or telemetry profiles.
