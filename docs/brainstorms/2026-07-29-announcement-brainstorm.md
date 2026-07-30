# thermalwriter public announcement — brainstorm

**Date:** 2026-07-29
**Status:** Channel set, timing, and lead angle decided by Mike; sequencing recommendation below.

## Where the project stands

- **v0.1.4 is live as the first non-prerelease "Latest" release** (2026-07-28). It was explicitly a launch-prep cut: glibc 2.35 baseline, vendored GUI fonts, MCP debug bridge stripped from release builds, tray auto-install, zero npm audit findings.
- **Both announcement gates are closed**: #91 (CPU 0.41% vs TRCC-Linux daemon 1.06%) and #90 (clean-machine L0+L1 install QA against tagged artifacts).
- **Collateral**: all three comparison charts favor thermalwriter on every axis (memory 81 MB vs 107–284 MB; CPU 0.41% vs 1.06–1.26%; install 20 MB vs 530–847 MB). Four layout GIFs + cava stream GIF in `docs/assets/gallery/`. Hardware photos of the Peerless Vision LCD are in progress.
- **Gap**: no channel plan, drafts, or timing existed before this document.

## Decisions already made (Mike, 2026-07-29)

1. **Channels**: Reddit Linux subs + press tips (Phoronix, Tom's Hardware, KitGuru "etc."), with AI-assisted-project policies checked per channel (research below).
2. **Timing**: polish pass first — AUR packaging (#87) and hardware photos land before any post.
3. **Lead angle**: the X11 streaming demo ("stream cava/conky/btop — any X11 app — to your cooler's LCD"). Footprint numbers support, don't lead.

## Research findings (2026-07-29, two web-research passes)

### AI-assisted-project policies — the headline finding

**No channel on the list prohibits covering or posting an AI-assisted project.**

- Every press AI policy found (Future plc/Tom's, GamingOnLinux, LWN) governs *their own newsroom writing*, not what software they'll cover.
- **Precedent in this exact niche**: TRCC-Linux's README openly credits Claude for protocol reverse-engineering and code generation — and Tom's Hardware covered it anyway (Aaron Klotz, **2026-02-07**, framed as a solo enthusiast scratching an itch). Note: project notes previously said "March 2026" for that coverage; the article page shows Feb 7.
- Reddit has no sitewide AI-disclosure rule. Per-sub AI rules cluster in art/image subs, not Linux/programming subs (Cornell/CHI 2025 taxonomy). The 2025 "no AI slop" backlash in programming subs targets low-effort, untested, drive-by project dumps — not assisted development per se.
- **Stance adopted**: write every post and pitch by hand in Mike's voice; don't foreground AI in any headline; don't conceal it if asked directly (the honest answer: heavily AI-assisted, human-architected, ~410 tests, measured footprint, hardware-verified — same posture TRCC took). The project's substance is the defense against "slop" pattern-matching.

### Reddit fit (caveat: research agent could not read live rule text — Reddit blocks bots; **live-verify sidebars for r/linux, r/linux_gaming, r/unixporn before posting**)

| Sub | Fit | Notes |
| --- | --- | --- |
| r/Thermalright | ✅ perfect topic, small | Actual device owners; unofficial fan sub, minimal rules; best tester-recruitment pool |
| r/linuxhardware | ✅ good, modest reach | Hardware-enablement projects welcome |
| r/opensource | ✅ built for this | OSS announcements explicitly welcome; disclose authorship |
| r/linux | ✅ biggest reach, high bar | FOSS release posts accepted; heavy AutoMod; needs a substantive technical writeup + author engagement, no editorialized title |
| r/linux_gaming | ⚠️ angle-dependent | Dev-friendly, but must be framed as "show live FPS/temps on your rig's cooler" with the gaming layout GIF, else off-topic risk |
| r/rust | ❌ skipped (Mike, 2026-07-29) | Research says showcases are welcome, but announcement threads there routinely code-review the repo; Mike isn't a Rust dev and can't engage on idiom-level questions, and that plus visible AI-assistance is the profile that draws pile-ons. r/linux covers most of the same reach |
| r/unixporn | rice showcase only | Photo of the riced desk *featuring the LCD* with required flair; never a project-announcement post |
| r/hardware | ❌ avoid | Self-promo/personal-project posts predictably removed |
| r/selfhosted | ❌ avoid | Off-topic (local hardware daemon, not a hosted service); also the one sub formalizing mandatory AI disclosure |

Universal Reddit norms: stagger posts across days (identical simultaneous cross-posts trip spam filters), tailor each title/body, post from the established personal account, be present in comments for the first few hours.

### Press fit

| Outlet | Verdict | Contact | Angle |
| --- | --- | --- | --- |
| **Phoronix** | Best fit | michael@phoronix.com | Benchmark-forward: measured 0.41%/81 MB footprint, Rust daemon, multi-transport USB/SCSI/HID, systemd/D-Bus, X11 streaming. Squarely Larabel's beat |
| **Tom's Hardware** | Proven appetite | Pitch Aaron Klotz directly (his TRCC piece, 2026-02-07: [article](https://www.tomshardware.com/pc-components/liquid-cooling/enthusiast-ports-thermalrights-lcd-software-for-windows-to-linux-fully-fledged-port-supports-a-ton-of-models-and-enables-rgb-and-lcd-customization)) | Natural follow-up: the lightweight always-on alternative in the niche he already covered |
| Hackaday | Good | tips@hackaday.com / [submit-a-tip](https://hackaday.com/submit-a-tip/) | Frame as a hack (reverse-engineered USB protocol, stream anything to a cooler), not a product launch — **lead the tip with the Doom clip**, it's the canonical Hackaday hook |
| GamingOnLinux | Good | contact@gamingonlinux.com | Gaming-rig aesthetic; strongly anti-AI-slop for their *own* content — lead purely with engineering |
| It's FOSS | Good | [news-tip form](https://itsfoss.com/contact-us-2/) | "New Linux app not yet covered" is literally a wanted topic |
| OMG! Ubuntu | Marginal | contact@omgubuntu.co.uk | Desktop-app angle; skews Ubuntu/GNOME |
| LWN | Deep-dive only | authors@lwn.net (pitch first) | Not an announcement outlet; a future technical article (multi-transport abstraction, D-Bus concurrency hardening) — paid freelance |
| **KitGuru** | **Skip (despite being on the wish list)** | Contact form only; explicitly ignores guest pitches | No Linux/OSS tooling lane found in their coverage |
| GamersNexus | Skip | — | Hardware testing/investigations; wrong format entirely |

## Approaches considered

**A. Staged rollout (recommended).** Polish gate → small friendly subs → main Reddit wave → press tips citing traction. Each stage shakes out problems (install bugs, FAQ gaps) before the next, larger audience sees them; press pitches land better with a traction signal (Tom's covered TRCC *after* it had Reddit momentum). Cost: the campaign spans ~1.5–2 weeks.

**B. Big-bang (everything within 48h).** Maximum concentrated momentum, GitHub-trending potential. Rejected: no learning loop between posts; a day-one install bug replays across every channel simultaneously; near-simultaneous cross-posts risk spam-filter removal.

**C. Press-first.** Tip Phoronix before Reddit; use coverage as social proof. Rejected: press may not bite for a niche tool with no traction signal, and the one proven data point in this niche ran in the opposite order.

## The plan (Approach A)

### Phase 0 — polish gate (this week; blocks everything)

1. **AUR package for the daemon** (#87). Arch/Hyprland users are a core audience; "not in the AUR" is the predictable first comment. GUI `-bin` package can follow later.
2. **Hardware photos land** (in progress) → README hero + post assets.
3. **Capture the video set** (confirmed in the marketing-pics session; Mike: "more than just cava if we're going to bother doing videos at all"). Shot list, all on the *physical* cooler in the real rig:
   - **cava** streaming, music audible/implied — the flashiest single clip.
   - **Gaming clip**: a game running while the LCD shows live FPS/temps climbing (fps-hero or neon-dash layout; MangoHud sensor provider makes the FPS real). This is the r/linux_gaming hero asset.
   - **btop or nvtop** streaming under load — the "any X11 app" proof point beyond visualizers.
   - **Doom, actually playable** on the cooler (required — Mike, 2026-07-29; demo-loop-only would disappoint r/itrunsdoom and Hackaday, whose bar is "plays," not "runs"). **VALIDATED END-TO-END on the live cooler 2026-07-29** — menu navigation, movement, and firing (ammo 50→49) all reacted on the physical LCD via XTEST injection into the daemon's Xvfb; exit restored layout mode/tick rate and cleaned up all processes. No daemon changes were needed. Working recipe (everything below is now installed):

     ```sh
     thermalwriter ctl mirror /usr/bin/doomretro -iwad /home/mike/doom/DOOM.WAD
     busctl --user set-property com.thermalwriter.Service /com/thermalwriter/display \
       com.thermalwriter.Display TickRate u 60    # 60 FPS for the clip (Mike's call)
     AUTH=$(ls $XDG_RUNTIME_DIR/thermalwriter/thermalwriter-xvfb-*/Xauthority)
     env -u WAYLAND_DISPLAY x11vnc -display :100 -auth "$AUTH" -nopw -bg
     vncviewer localhost:0   # focus this window, play keyboard-only, eyes on the cooler
     thermalwriter ctl layout svg/neon-dash-v2.svg   # done — restores layout AND 2 FPS tick
     ```

     Notes from validation (all re-verified with the real assets at 60 FPS): **real `DOOM.WAD` (Ultimate Doom) + `DOOM2.WAD` copied from Mike's Steam library to `~/doom/`** — authentic E1M1 confirmed on the panel; **audio works** (doomretro shows up as a PipeWire sink-input from the daemon-spawned child — game music/sfx play through desktop audio for the camera); **runtime `TickRate` set-property works mid-stream** (note the property lives on iface `com.thermalwriter.Display`, not `.Service`) and exiting the stream restores the 2 FPS layout tick; **`chocolate-doom` is not in the Arch repos — `doomretro` is** (installed, software-rendered, fullscreens the square panel); **x11vnc 0.9.17 refuses to start if `WAYLAND_DISPLAY` is set** even when pointed at an X display — the `env -u` above is load-bearing; Ultimate Doom's menu needs four Enters to reach E1M1 (episode select). `~/doom/freedoom1.wad` remains as a licensing-clean fallback. Fallback if anything misbehaves on the day: attract-mode demo loop / `-playdemo`. Copy note: never quote the 0.41% CPU figure alongside 60 FPS streaming — footprint claims are layout-mode numbers.
   - **Layout switch**: tray/GUI flipping between designed layouts (neon-dash-v2 with background) to show the daemon side, not just streaming.
   - Cut per channel: a ~20–30s montage for the README/press kit, the gaming cut for r/linux_gaming, the layouts cut for r/Thermalright. Rendered GIFs remain the fallback.
   - **Case lighting coordination** (Mike, 2026-07-29 — unibody fan above the cooler + motherboard lighting array to the left must look cohesive in every shot). **VALIDATED**: OpenRGB drives the Gigabyte X870E AORUS MASTER's ARGB v2 headers (fan + array), the board accent zones, and the RTX 5080's side lighting; Mike's curated `tokyo-night.orp` profile loads cleanly and the per-clip override + restore round-trip works. Per-clip plan:
     - **Hard-won lesson (2026-07-29): OpenRGB "Direct" mode does not persist — it's a software-driven mode that needs a continuously running client feeding frames.** Fire-and-forget CLI invocations (or profile loads of a Direct-mode profile) leave the controller feedless on exit; what stays lit is undefined. Hardware modes (**Static**, breathing, etc.) are written to the controller and persist. Working one-liners for the shoot, verified on the IT5711: `openrgb -d 2 -m static -c 7AA2F7` (Tokyo Night blue, whole board incl. fan/array) and `openrgb -d 0 -m static -c FF1A00 -d 2 -m static -c FF1A00` (Doom red, GPU + board). Also: the saved `tokyo-night.orp` predates the ARGB zones being sized (0 LEDs at save time) so it never covered the fan/array. **Pre-shoot task for Mike (GUI, once): size the ARGB zones, set colors, switch the board device to Static mode, and re-save `tokyo-night.orp` (+ a `doom-red.orp`)** — then profile loads are genuine persistent one-command restores.
     - **Stills, layout/tray, cava, btop clips — the "brand look" (final, Mike-approved 2026-07-29)**: `openrgb -d 2 -m static -c 7A2BF7` — saturated Tokyo Night purple on the whole board (fan + array + accents). Chosen over the literal theme hexes because LEDs render pastels (`7aa2f7`, `bb9af7`) near-white; `7A2BF7` cuts green and keeps blue dominant so it reads clearly purple on camera. GPU side lighting stays on Mike's profile colors (optionally `openrgb -d 0 -m static -c 7A2BF7` for full uniformity — Mike's call at the shoot).
     - **Doom clip**: `openrgb -d 0 -m static -c FF1A00 -d 2 -m static -c FF1A00` — slightly orange red reads instantly and blooms less on camera than pure FF0000. Restore after with the purple command above.
     - **Gaming FPS clip**: match the layout on screen — if fps-hero's red/orange gaming accents lead, use the Doom-red scheme; if neon-dash-v2, stay Tokyo Night.
     - **Never use rainbow/cycle modes on camera** — cycling fights the LCD for attention and PWM/color transitions can strobe against the camera shutter. Static colors only while rolling.
4. **README hero reorder**: streaming currently sits at 280px in the gallery table; if streaming leads the pitch, move the stream GIF (or hardware shot) to the top of Features.
5. **Live-verify subreddit rules** in a browser: r/linux, r/linux_gaming, r/unixporn (research agent couldn't read live rule text).
6. Pre-draft the FAQ comment answers (below).
7. Housekeeping: correct the TRCC-coverage date (Feb 2026, not March) in CLAUDE.md/memory.

### Phase 1 — soft launch (days 1–2)

Post to **r/Thermalright**, **r/linuxhardware**, **r/opensource** (staggered, tailored). Small, friendly, on-topic audiences; doubles as tester recruitment (9 device IDs supported, 1 hardware-verified). Watch for install failures and FAQ gaps; fix before Phase 2.

### Phase 2 — main wave (days 3–6)

- **r/linux**: substantive technical writeup — architecture, measured footprint with methodology link, TRCC credit, streaming demo up top.
- **r/linux_gaming** (separate day): gaming-rig framing — live FPS/temps/cava on the cooler, fps-hero/neon-dash GIF.
- **r/unixporn**: not in this wave — save the riced-desk photo featuring the LCD as ambient long-tail whenever a good shot exists.

### Phase 3 — press tips (after Phase 2 traction, ~day 7–10)

- **Phoronix** first: short benchmark-forward email to Michael Larabel with repo, comparison methodology, gallery GIFs, hardware photos.
- **Tom's Hardware**: pitch Aaron Klotz as the follow-up to his TRCC piece; cite Reddit reception.
- **Hackaday + It's FOSS + GamingOnLinux** in parallel, each with the tailored angle above.
- OMG! Ubuntu optional. KitGuru/GamersNexus skipped per research (Mike can override — surfaced explicitly since KitGuru was on the original wish list).

### Phase 4 — long tail

- **r/itrunsdoom**: the Doom clip as its own post ("Thermalright CPU cooler LCD runs Doom") — purpose-built audience, meme-positive, zero self-promo friction; link the repo in a comment. Can run any time after the clip exists; also reusable as a second-wind post if the main wave underperforms.
- LWN technical deep-dive pitch (someday; paid, substantial writing commitment).
- r/unixporn rice showcase whenever the desk shot is ready.

## Post content blueprint

Every post: human-written, tailored; hardware photo or streaming GIF at top; 2–3 sentences on what it is; streaming as hook; footprint chart as supporting evidence (never the headline claim); TRCC-Linux credited by name as the feature-rich option and protocol-table source; testers-wanted call; no "first/only" claims anywhere.

Draft title directions (to be refined per sub at writing time):

- r/Thermalright: "I built a lightweight Linux daemon for the Peerless Vision LCD — it can stream cava/btop to the cooler. Testers wanted for other models"
- r/linux: "thermalwriter: a Rust daemon for Thermalright LCD coolers — designed sensor layouts, or any X11 app streamed to the display"
- r/linux_gaming: "Your AIO's LCD can show live FPS and temps — or run cava — from a 0.4%-CPU background daemon"

## Prepared Q&A (pre-draft before Phase 1)

- **"Why X11/Xvfb and not Wayland?"** — the capture source is a *hidden virtual* Xvfb framebuffer, independent of the desktop session; your desktop can be (and the dev machine is) Wayland/Hyprland. The streamed apps just need to speak X11.
- **"Was this AI-generated?"** — honest, non-defensive: heavily AI-assisted, human-architected and reviewed, ~410 tests, measured footprint, clean-machine install QA, hardware-verified on the dev cooler. Same openly-stated posture as TRCC-Linux, which this project credits.
- **"How is this different from TRCC-Linux?"** — measured, generous framing straight from the README: TRCC owns breadth (devices/LED/video); thermalwriter trades breadth for a minimal always-on footprint and X11 streaming. Link comparison methodology.
- **"Does it work on my cooler?"** — support table; 9 IDs, 1 hardware-verified; open a device report issue.
- **"Why not just use TRCC?"** — never dunk; "you probably should if you want LED/video/breadth" + the footprint numbers for the always-on case.

## Resolved questions (Mike, 2026-07-29)

1. **r/rust: skipped.** Mike isn't a Rust dev and doesn't want to defend idiom-level code review in an announcement thread; r/linux covers most of the same reach. (This Week in Rust dropped from Phase 4 as a consequence.)
2. **Posting account: Mike's established Reddit account.** Satisfies AutoMod karma/age gates.
3. **Videos: confirmed, expanded beyond cava** — full shot list in Phase 0 item 3 (cava, gaming/FPS clip, btop/nvtop under load, layout switching), cut per channel.
