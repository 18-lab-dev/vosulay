# Code signing policy

Windows releases are prepared for signing through the
[SignPath Foundation](https://signpath.org/) open-source programme:

> Free code signing provided by [SignPath.io](https://signpath.io/), certificate by
> [SignPath Foundation](https://signpath.org/).

Signing is **not active yet**. Until SignPath approves the project and the
repository settings below exist, the release workflow builds the same unsigned
packages it always has.

## Team roles

- Committers and reviewers: [members of the shiruviaa organization](https://github.com/orgs/shiruviaa/people)
- Approvers: [owners of the shiruviaa organization](https://github.com/orgs/shiruviaa/people?query=role%3Aowner)

Every team member uses multi-factor authentication for GitHub and SignPath.
Changes from people outside the team are reviewed by a team member before they
are merged. An approver approves each signing request.

## Privacy policy

This program will not transfer any information to other networked systems
unless specifically requested by the user or the person installing or operating
it. Vosulay connects to the vosu or tosu server the user configures (by default
on the same computer) and loads the overlays the user installs; those overlays
are third-party web pages whose own network behaviour is up to their authors.
The full behaviour, including listeners, injection and input access, is in the
[security and installation disclosure](security.md).

## What is signed

SignPath Foundation's terms let a project sign only binaries built from its own
source ([“Sign your own binaries only”](https://signpath.org/terms.html)).
Upstream binaries may ship unsigned inside a signed installer.

| File | Built from | Signed |
| --- | --- | --- |
| `vosulay.dll` | this repository | yes |
| `vosulay-<version>-windows-x86_64-setup.exe` | Inno Setup compiling [`packaging/windows/vosulay.iss`](../packaging/windows/vosulay.iss) | yes |
| `vosulay.exe` | CEF's prebuilt `bootstrap.exe`; only its icon and version resources are changed | no: upstream binary |
| `libcef.dll` and the other CEF runtime files | CEF's prebuilt distribution | no: upstream |
| `asdf_overlay-x64.dll`, `asdf_overlay-x86.dll` | asdf-overlay's release prebuilts | no: upstream |

Origins and licences for every shipped file are in [provenance](provenance.md).
All three vosulay-named files carry product name `Vosulay` and the release
version in their version resources (`build.rs` links one into the DLL;
`vosulay.iss` sets the installer's), as SignPath's metadata restrictions require.

## How releases are signed

The **Packages** workflow ([`.github/workflows/release.yml`](../.github/workflows/release.yml))
signs only when it runs on a `v<version>` tag and the repository variable
`SIGNPATH_ORGANIZATION_ID` is set. Its Windows job then:

1. builds `vosulay.dll` and submits it to SignPath (artifact configuration
   `client-dll`), waiting for approval;
2. packages the **signed** DLL with
   `vosulay-cef package-windows dist/package --client <signed vosulay.dll>`,
   because the installer embeds the DLL and the package manifest records its
   digest, so the DLL cannot be signed afterwards;
3. compiles the installer and submits it (artifact configuration `installer`);
4. runs [`packaging/windows/verify-release.ps1`](../packaging/windows/verify-release.ps1)
   with `-RequireSignature`, which checks product name and version on all three
   files, valid Authenticode signatures on the DLL and installer, and that the
   ZIP holds the same DLL as the installer.

Unsigned runs use the same steps without submissions and verify without
`-RequireSignature`.

## Enabling signing

Prerequisites outside this repository, for maintainers:

1. The repository must be public; SignPath Foundation signs open-source
   projects only. Enable GitHub private vulnerability reporting at the same time
   ([`SECURITY.md`](../SECURITY.md) relies on it).
2. Apply to SignPath Foundation and create the project `vosulay` with signing
   policy `release-signing`, connected to this GitHub repository as a trusted
   build system.
3. Add two artifact configurations. GitHub artifacts arrive as ZIP files:

   `client-dll`:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="vosulay.dll" product-name="Vosulay" original-filename="vosulay.dll">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

   `installer`:

   ```xml
   <artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
     <zip-file>
       <pe-file path="vosulay-*-windows-x86_64-setup.exe" product-name="Vosulay">
         <authenticode-sign />
       </pe-file>
     </zip-file>
   </artifact-configuration>
   ```

   Inno Setup pads its version strings with trailing spaces. Windows shows them
   trimmed; check on the first signing request that SignPath's `product-name`
   restriction accepts the installer.
4. Add the repository variable `SIGNPATH_ORGANIZATION_ID` and the secret
   `SIGNPATH_API_TOKEN`.

The next tagged **Packages** run then requests both signatures.
