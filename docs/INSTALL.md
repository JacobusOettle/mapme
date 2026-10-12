# Installing MapMe on your iPhone

There are three ways to get MapMe onto a phone. **A** needs only a free Apple ID
and a Linux, Windows or Mac computer. **B** needs a paid Apple Developer account
and no computer at all after setup (setup works from Linux too). **C** needs a
Mac with Xcode. You never need a Mac of your own: GitHub's macOS runners build
the app on every push, and A or B put that build on your phone.

| | What you need | App lasts | Effort |
|---|---|---|---|
| A. Sideload the CI build | free Apple ID, Linux/Windows/Mac, USB cable | 7 days, then re-sign | 10 min |
| B. TestFlight | Apple Developer Program ($99/yr) | 90 days per build, updates itself | 30 min once, then automatic |
| C. Build in Xcode | Mac with Xcode 16+ | 7 days (free) / 1 year (paid) | 15 min |

Renting a cloud Mac does not help with A: a free Apple ID can only sign for a
phone that is plugged into the computer doing the signing, and cloud Macs are
rented in 24-hour blocks.

## A. Sideload the unsigned IPA (free Apple ID)

### 1. Download the IPA

The repository is private, so sign in to GitHub first.

* **Latest build (easiest):** every CI build of any branch puts its IPA on the
  `ci-screenshots` branch:
  [MapMe-unsigned.ipa](https://github.com/JacobusOettle/mapme/raw/ci-screenshots/MapMe-unsigned.ipa)
  (direct download; the branch also has the screenshots of that build).
* **Release:** open
  [Releases → latest-build](https://github.com/JacobusOettle/mapme/releases/tag/latest-build)
  and download `MapMe-unsigned.ipa`. CI refreshes this prerelease after every
  green build of `main` or a `v*` tag.
* **Any CI run:** Actions → **iOS** → click a run → *Artifacts* →
  `MapMe-unsigned-ipa`. This downloads a `.zip`; unzip it to get the `.ipa`.
* **Publish a branch build as the release:** Actions → **iOS** → *Run workflow* →
  pick the branch → tick *Publish the IPA as the "latest-build" prerelease*.
  (GitHub shows *Run workflow* once the workflow file is on `main`.)
* **Command line:**
  `gh release download latest-build -R JacobusOettle/mapme -p MapMe-unsigned.ipa`

### 2a. Install it from Linux

Two open-source tools sign an IPA with your Apple ID and install it over USB on
Linux. Both need `usbmuxd` (most distributions have it; otherwise
`sudo apt install usbmuxd` / `sudo dnf install usbmuxd`; plug the phone in
*before* starting the tool, because usbmuxd stops when no device is connected).

* **[PlumeImpactor](https://github.com/khcrysalis/PlumeImpactor)** (on
  Flathub): connect the iPhone, unlock it, tap **Trust**, sign in with your
  Apple ID, pick `MapMe-unsigned.ipa` and install. On Linux it re-signs
  automatically when the phone is plugged in (usbmuxd has no Wi-Fi), so plug it
  in once a week.
* **[iloader](https://github.com/nab138/iloader)** (packages on its releases
  page, AUR `iloader-bin`, Fedora COPR, a NixOS flake): sign in, then either
  import `MapMe-unsigned.ipa` directly, or install **SideStore** first. SideStore
  re-signs apps on the phone itself, so after this one-time setup you can add
  MapMe from the IPA in Files and refresh it every 7 days without a computer.

Then trust the developer and turn on Developer Mode (steps 4 and 5 below). These
are third-party tools that talk to Apple with your Apple ID; consider a
secondary Apple ID. Logs: iloader keeps them in
`~/.local/share/me.nabdev.iloader/logs/`.

### 2b. Install it with Sideloadly (Windows or macOS)

1. Install [Sideloadly](https://sideloadly.io). On Windows, also install
   **iTunes and iCloud from apple.com** (not the Microsoft Store versions).
2. Connect the iPhone with a cable, unlock it and tap **Trust**.
3. Drag `MapMe-unsigned.ipa` into Sideloadly, type your Apple ID and press
   **Start**. Enter the password and the two-factor code when asked. (Consider a
   secondary Apple ID; Sideloadly only talks to Apple with it.)
4. On the iPhone: **Settings → General → VPN & Device Management →** your Apple
   ID **→ Trust**.
5. iOS 16 and later: **Settings → Privacy & Security → Developer Mode → On**,
   restart, then confirm **Turn On**. (The switch appears after the first
   sideloaded app is installed.)

### …or with AltStore (Classic)

1. Install AltServer from [altstore.io](https://altstore.io) on your Mac or PC
   (Windows needs the apple.com iTunes and iCloud too), then use it to install
   AltStore on the iPhone with your Apple ID.
2. Trust the profile and enable Developer Mode as in steps 4 and 5 above.
3. Copy `MapMe-unsigned.ipa` to the phone (AirDrop, iCloud Drive, Files), open
   AltStore → **My Apps → +** and pick the IPA.

### Every 7 days

Apps signed with a free Apple ID stop opening after 7 days. Re-install the same
way (installing over the existing app keeps your maps and tracks), or let it
refresh itself: SideStore refreshes on the phone, PlumeImpactor on Linux when the
phone is plugged in, and Sideloadly's *auto-refresh* and AltStore's background
refresh while the computer is on the same Wi-Fi. A free Apple ID can have 3
sideloaded apps at a time.

## B. TestFlight (paid Apple Developer account)

CI has a **TestFlight upload (signed)** job that stays skipped until the secrets
below exist. With them, every green build of `main`, every `v*` tag and every
manual run with *publish* ticked is signed and uploaded to TestFlight.

1. In [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources):
   * **Identifiers → +**: an explicit App ID `com.jacobusoettle.mapme` (or your
     own; then set the repository *variable* `APP_BUNDLE_ID` to it).
   * **Certificates → +**: an *Apple Distribution* certificate (create the CSR
     with Keychain Access → Certificate Assistant). Install it, then in Keychain
     Access → *My Certificates* right-click it → **Export** as `.p12` with a
     password. **On Linux**, make the CSR and the `.p12` with OpenSSL instead:

     ```sh
     openssl req -new -newkey rsa:2048 -nodes -keyout distribution.key \
       -out distribution.csr -subj "/emailAddress=you@example.com/CN=Your Name/C=ZA"
     # upload distribution.csr, download distribution.cer, then:
     openssl x509 -inform DER -in distribution.cer -out distribution.pem
     openssl pkcs12 -export -legacy -inkey distribution.key -in distribution.pem \
       -out distribution.p12 -passout pass:CHOOSE_A_PASSWORD
     ```

     (`-legacy` writes the older encryption that every macOS keychain imports;
     leave it out on OpenSSL 1.x. Keep `distribution.key` private.)
   * **Profiles → +**: *App Store Connect* distribution profile for that App ID
     and certificate. Download the `.mobileprovision`.
2. In [App Store Connect](https://appstoreconnect.apple.com): **Apps → + New App**
   (iOS, name MapMe, the bundle ID above). Then **Users and Access →
   Integrations → App Store Connect API → Team Keys → +** with the *App Manager*
   role. Download `AuthKey_<KEYID>.p8` (only possible once) and note the Key ID
   and Issuer ID.
3. In the GitHub repository: **Settings → Secrets and variables → Actions → New
   repository secret**:

   | Secret | Value |
   |---|---|
   | `APPLE_TEAM_ID` | 10-character Team ID (developer.apple.com → Membership) |
   | `APPLE_CERTIFICATE_P12_BASE64` | `base64 -i distribution.p12` |
   | `APPLE_CERTIFICATE_PASSWORD` | the `.p12` export password |
   | `APPLE_PROVISIONING_PROFILE_BASE64` | `base64 -i MapMe_AppStore.mobileprovision` |
   | `ASC_KEY_ID` | API Key ID |
   | `ASC_ISSUER_ID` | API Issuer ID |
   | `ASC_KEY_P8_BASE64` | `base64 -i AuthKey_<KEYID>.p8` |

   (`base64 -i file | pbcopy` copies it on a Mac; on Linux use `base64 -w0 file`;
   in PowerShell `[Convert]::ToBase64String([IO.File]::ReadAllBytes("file"))`.)
   Optional repository *variables*: `APP_BUNDLE_ID`, `XCODE_VERSION` (e.g. `16.4`).
4. Push to `main` (or run the **iOS** workflow with *publish* ticked). After the
   tests pass, the job archives, signs, exports and uploads with `xcrun altool`.
   The build number is `<run number>.<attempt>`.
5. The build appears in App Store Connect → TestFlight after processing (5 to 30
   minutes). Everything in B is done in a browser plus the OpenSSL commands
   above, so it works from Linux. Add yourself under *Internal Testing*, install the **TestFlight**
   app on the iPhone and accept the invite. The app declares no non-exempt
   encryption, so there is no export-compliance question.

## C. Build it yourself on a Mac

```sh
# Xcode 16 or newer from the App Store, plus:
brew install xcodegen rustup
export PATH="$(brew --prefix rustup)/bin:$PATH"   # Homebrew's rustup is keg-only
rustup default stable
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
# (Intel Mac simulator: also x86_64-apple-ios and INCLUDE_X86_SIM=1 below)

scripts/build-core-ios.sh            # Rust core -> build/MapMeCore.xcframework
cd ios && xcodegen generate          # -> ios/MapMe.xcodeproj (generated, not committed)
open MapMe.xcodeproj
```

In Xcode select the **MapMe** target → **Signing & Capabilities**, tick
*Automatically manage signing* and choose your **Team** (your Apple ID's
*Personal Team* works). If Xcode says the bundle identifier is taken, change it
to something like `com.yourname.mapme`. Plug in the iPhone, pick it as the run
destination and press **⌘R**. Trust the developer and enable Developer Mode as
in A.4 and A.5.

Run `scripts/build-core-ios.sh` again after changes in `core/`, and
`xcodegen generate` after pulling changes that add or remove Swift files.
`make help` lists shortcuts.

## Using MapMe with the Grootkloof map

1. **Get the PDF onto the phone**: AirDrop, email, iCloud Drive or any chat app.
2. **Open it in MapMe**: in Files (or Mail), tap the PDF → **Share** → **MapMe**.
   Or open MapMe → **Import a map** → *PDF or image from Files* and pick it.
   Screenshots and images work the same way (*Photo or screenshot*).
3. **Allow location access** (*While Using the App*; *Always* only if you want
   track recording to continue with the screen off).
4. **MapMe locates the map by itself.** It renders the PDF, reads the text on
   it and, for the Grootkloof sheet, finds the 1-arc-minute grid labels
   (25°49'S, 27°23'E, ...): located in a few seconds, no internet needed.
   Maps without coordinates are matched by their contour lines against real
   terrain near your GPS position; this downloads elevation tiles once (internet
   needed) and first searches within the *Terrain search radius* in Settings
   (50 km by default). If the map is farther away, **Search wider…** on the
   result screen (or in the library menu of the map) looks up to 2 000 km out,
   checking the places named on the map first, and says beforehand how much it
   will download.
5. **On the map**: the red dot is you, the halo is the GPS accuracy and the cone
   is your heading. The orientation button switches between *North up*,
   *Heading up* (the map turns with you) and *Free rotate*. Tap **Record** for
   the dotted trail, and open **Nearby** for peaks, spot heights and places
   from the map and OpenStreetMap, with distance and direction.
6. **If it cannot locate the map** (or you want to correct it), choose
   **Calibrate manually**: tap a point on the map, then the same point on the
   satellite view (or tap **I'm here** where you stand). Two points give a
   rough fit, three or more a good one.

Trying it at home: in the iOS Simulator use **Features → Location → Custom
Location** with latitude `-25.85`, longitude `27.40` to stand in the middle of
the Grootkloof sheet.
