# Code signing policy

Windows releases are prepared for signing through the
[SignPath Foundation](https://signpath.org/) open-source programme:

> Free code signing provided by [SignPath.io](https://signpath.io/), certificate by
> [SignPath Foundation](https://signpath.org/).

Signing is **not active yet**. Until SignPath approves the project and the
repository settings below exist, the release workflow builds the same unsigned
packages it always has.

## Team roles

- Committers, reviewers and approvers: [qt715](https://github.com/qt715)

K4zoku, the author of the prototypes vosulay grew from, also holds repository
administrator access. Every team member must use multi-factor authentication for
GitHub and SignPath. Changes from anyone else must be reviewed by qt715 before
they are merged. Every release signing request must be manually approved by
qt715 through the `release-signing` policy; automated approval is not permitted.

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
   files, valid Authenticode signatures with signer subject `SignPath Foundation`
   and a timestamp countersignature on the DLL and installer, and that the
   ZIP's DLL matches the staged `dist/package/vosulay.dll` the installer is
   compiled from. It does not unpack the installer, so the installer's embedded
   copy is not compared independently.

Unsigned runs use the same steps without submissions and verify without
`-RequireSignature`.

Release jobs use fresh GitHub-hosted runners and do not restore Cargo `target`
or other build-output caches from any ref. This keeps signing inputs built from
the checked-out source rather than reused build outputs. Dependency downloads
are checked against Cargo.lock or the pinned CEF/asdf digests before use.

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

## Maintainer pre-release checklist

The following service-side settings are **unverified from this repository**.
The workflow's wait-for-completion option does not prove manual approval,
trusted origin verification, or metadata restrictions. Before the first signed
release, maintainers must confirm and record:

- [ ] SignPath Foundation acceptance, public repository/download visibility,
  and eligibility of the disclosed injection/hooks and compatibility settings.
- [ ] `release-signing` requires manual approval by qt715 for **each** DLL and
  installer request; no automatic approval path is enabled.
- [ ] SignPath trusts this repository's GitHub Actions origin and verifies the
  submitted artifact's originating workflow/commit/tag.
- [ ] Both artifact configurations restrict the product name to `Vosulay`,
  match the intended filenames, and enforce release-version metadata consistent
  with the tag (including acceptance of Inno Setup's padded strings).
- [ ] All team members have GitHub and SignPath MFA enabled. GitHub reported
  MFA enabled for K4zoku and qt715 on 2026-10-10, but the shiruviaa organization
  did not require it; the owner must enforce it. SignPath MFA remains unverified.
- [ ] A real signed DLL and installer pass `verify-release.ps1 -RequireSignature`
  on Windows; authorized native install/upgrade/uninstall checks pass.
- [ ] The unresolved upstream provenance/licensing items in
  [provenance](provenance.md#signing-boundary-and-open-maintainer-items) are settled,
  private vulnerability reporting is enabled, and release notes include the
  Code signing policy, attribution, and privacy/disclosure links.

Checking in this policy is not evidence that any of these service-side settings
has been configured or that Foundation eligibility has been approved.
