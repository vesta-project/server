# Resource sides and embedded software changes

Resources now show Client, Server, Client & server, World, or Side unknown.
JAR declarations take priority over explicit provider side requirements. Plain
provider booleans without side requirements are not treated as authoritative
side evidence. Plugins are server resources; datapacks are world resources.
Listing reads bounded JAR metadata independently of its full-file hash budget.
Pending disk-backed pack JARs expose their own declaration before activation.
Fabric's omitted environment defaults to all physical environments, matching
https://docs.fabricmc.net/develop/loader/fabric-mod-json#environment.

The pack dialog now embeds the existing software ChangeFlow, prefilled with the
pack's software, Minecraft version and loader version. It reuses normal plan/apply
confirmations, warning acknowledgements, stopped-state guards and Java checks.
It skips version-catalogue loading for this fixed target. The dialog follows the
accepted job with bounded polling and refreshes/replans the pack after successful
software activation. Failed jobs stay visible and do not apply the pack. Java
jobs refresh runtime information without being mistaken for a software switch.

Validation: frontend typecheck passed; 25 focused content/hosting/pack/localization
tests passed, including side precedence and unknown-side behavior. A new Rust
staged-JAR metadata test passed. Three JAR metadata tests passed, including
Fabric's default and explicit client-only declarations. No real Minecraft
process or native visual session was exercised.

Backend metadata changes require rebuilding the agent. Deployment was not
retried against the previously empty dev-server registry. Frontend changes are
source-complete and reuse existing styles and components.
