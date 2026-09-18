# Demo device

A fictional device producing measurements, to explore the tool without
hardware. No hardware required.

## Setup

1. Give it any name and confirm.
2. Fake measurements will appear right away, enough to browse the interface.

The address is free (the placeholder says `demo`), there is no credential and
no option. The demo device produces a `cpu_usage_percent` series, so the
"High CPU" and "Unusual CPU" rules apply to it, and it can be used as a parent
or a child in a [dependency chain](../alerting/index.md#dependency-suppression)
to see suppression at work.

For real measurements without dedicated hardware, point an SNMP device at any
machine running `snmpd`: see [Testing without hardware](../install/docker.md#testing-without-hardware).
