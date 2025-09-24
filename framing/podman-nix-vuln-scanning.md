OpenSCAP and its utility oscap-podman are free and open-source tools for container vulnerability and compliance scanning. OpenSCAP is licensed under the GNU Lesser General Public License (LGPL) version 2.1 or later, making it a permissive open-source license widely used in open-source projects.[1][2][9]

I will now check the licensing status of the other tools like vulnix, Aqua Trivy, Snyk Container, Anchore, and Grype to provide a full list of the free and open-source options.I will now search for the licensing information of these tools: vulnix, Aqua Trivy, Snyk, Anchore Engine, and Grype.{"tool_uses":[{"recipient_name":"functions.search_web","parameters":{"queries":["vulnix license open source"]}},{"recipient_name":"functions.search_web","parameters":{"queries":["Aqua Trivy open source license"]}},{"recipient_name":"functions.search_web","parameters":{"queries":["Snyk container vulnerability scanning license"]}},{"recipient_name":"functions.search_web","parameters":{"queries":["Anchore Engine open source license"]}},{"recipient_name":"functions.search_web","parameters":{"queries":["Grype container scanner open source license"]}}]}Here are the licensing details for the common container CVE and vulnerability scanning tools:

- vulnix: It is open source maintained by the Nix community, licensed under the MIT license, which is a permissive open source license.[11]

- Aqua Trivy: An open-source tool licensed under the Apache License 2.0, free for use and modification.[12]

- Snyk Container: Snyk offers commercial products with free tiers, but its core container scanning engine is not fully open source; parts may be proprietary or offered under commercial terms.[12]

- Anchore Engine: Open source licensed under the Apache License 2.0, suitable for free use in builds and CI.[12]

- Grype: A fast and open-source vulnerability scanner for container images, licensed under the Apache License 2.0.[12]

Summary table of free and open-source tools for container CVE scanning:

| Tool           | License           | Open Source | Notes                        |
|----------------|-------------------|-------------|------------------------------|
| OpenSCAP/oscap-podman | LGPL 2.1+         | Yes         | Free and open source          |
| vulnix         | MIT               | Yes         | Nix-focused CVE scanner       |
| Aqua Trivy     | Apache 2.0        | Yes         | Widely used container scanner |
| Anchore Engine | Apache 2.0        | Yes         | Open source vulnerability scanner |
| Grype          | Apache 2.0        | Yes         | Fast scanning tool            |
| Snyk Container | Commercial/Proprietary | No          | Partial free tier, not fully open source |

These open-source tools can be integrated into CI pipelines for vulnerability enforcement in container builds using Podman and Nix ecosystems.[13][1][11][12]

[1](https://www.redhat.com/en/blog/container-vulnerabilities-openscap)
[2](https://git.assignitapp.com/open-source/openscap/openscap/-/blob/maint-1.3/openscap.spec)
[3](https://fossies.org/linux/openscap/utils/oscap-podman)
[4](https://opendev.org/starlingx/integ/commit/f043585c651e342b9fbf5335811604e013d829db)
[5](https://static.open-scap.org/openscap-1.3/oscap_user_manual.html)
[6](https://docs.oracle.com/cd/F61088_01/oscap/OL9-OSCAP.pdf)
[7](https://build.opensuse.org/projects/openSUSE:Leap:15.6:Update/packages/openscap/files/openscap.spec?expand=1)
[8](https://docs.oracle.com/en/operating-systems/oracle-linux/8/oscap/OL8-OSCAP.pdf)
[9](https://search.nixos.org/packages?channel=unstable&show=openscap&size=50&sort=relevance&type=packages&query=openscap)
[10](https://github.com/OpenSCAP/openscap/releases)
[11](https://github.com/nix-community/vulnix)
[12](https://www.sentinelone.com/cybersecurity-101/cybersecurity/container-vulnerability-scanning-tools/)
[13](https://dev.to/orhillel/best-5-tools-to-help-eliminate-cves-from-container-images-1p2c)
