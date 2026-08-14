<script lang="ts">
  import type { PreviewProfile, PreviewProfileId } from "./types";
  import { PREVIEW_PROFILES } from "./types";

  type Props = {
    selected?: PreviewProfileId;
    onchange?: (profile: PreviewProfile) => void;
    disabled?: boolean;
  };

  let {
    selected = $bindable<PreviewProfileId>("square"),
    onchange = () => {},
    disabled = false,
  }: Props = $props();

  function selectProfile(profile: PreviewProfile) {
    if (disabled) return;
    selected = profile.id;
    onchange(profile);
  }
</script>

<section class="composer-section profile-selector" aria-labelledby="profile-selector-title">
  <div class="composer-section-heading">
    <div>
      <p class="eyebrow">Native surface</p>
      <h2 id="profile-selector-title">Preview profile</h2>
    </div>
    <span class="composer-count">{PREVIEW_PROFILES.length} targets</span>
  </div>

  <div class="profile-grid" role="group" aria-label="Native display profiles">
    {#each PREVIEW_PROFILES as profile (profile.id)}
      <button
        type="button"
        class="profile-option"
        class:selected={selected === profile.id}
        data-profile={profile.id}
        data-native-width={profile.width}
        data-native-height={profile.height}
        aria-pressed={selected === profile.id}
        aria-label={`${profile.label}, ${profile.width} by ${profile.height} native pixels`}
        title={profile.description}
        onclick={() => selectProfile(profile)}
        disabled={disabled}
      >
        <span class="profile-option-shape profile-shape-{profile.id}" aria-hidden="true"></span>
        <span class="profile-option-copy">
          <strong>{profile.label}</strong>
          <span>{profile.width} × {profile.height}</span>
          <small>{profile.topology === "curved-panorama" ? "Curved panorama" : "Rectangular surface"}</small>
        </span>
      </button>
    {/each}
  </div>

  <p class="profile-help">
    Preview uses the selected surface's native pixels; the window only scales presentation.
    Curved guides are illustrative and do not claim calibrated optical warp.
  </p>
</section>

<style>
  .profile-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }

  .profile-option {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
    padding: 9px;
    color: var(--text-muted);
    text-align: left;
    background: var(--bg-base);
    border: 1px solid var(--line-soft);
    border-radius: var(--radius-md);
    transition: border-color 120ms ease, background 120ms ease, color 120ms ease;
  }

  .profile-option:hover:not(:disabled) {
    color: var(--text-primary);
    background: var(--bg-elev);
    border-color: var(--line-strong);
  }

  .profile-option.selected {
    color: var(--text-primary);
    background: color-mix(in srgb, var(--accent) 11%, var(--bg-base));
    border-color: var(--accent);
    box-shadow: 0 0 0 1px color-mix(in srgb, var(--accent) 20%, transparent);
  }

  .profile-option:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .profile-option:disabled {
    cursor: wait;
    opacity: 0.58;
  }

  .profile-option-shape {
    flex: 0 0 auto;
    width: 26px;
    height: 26px;
    border: 1px solid currentColor;
    border-radius: 4px;
    opacity: 0.72;
  }

  .profile-shape-portrait {
    height: 34px;
    width: 16px;
  }

  .profile-shape-wide {
    height: 16px;
    width: 34px;
  }

  .profile-shape-curved {
    width: 34px;
    height: 18px;
    border-radius: 50% / 30%;
    border-style: dashed;
  }

  .profile-option-copy {
    display: flex;
    flex: 1 1 auto;
    min-width: 0;
    flex-direction: column;
    gap: 2px;
  }

  .profile-option-copy strong,
  .profile-option-copy span,
  .profile-option-copy small {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .profile-option-copy strong {
    color: currentColor;
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .profile-option-copy span {
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: 10px;
  }

  .profile-option-copy small {
    color: var(--text-dim);
    font-size: 9px;
  }

  .profile-help {
    margin: 0;
    color: var(--text-dim);
    font-family: var(--font-mono);
    font-size: 9.5px;
    line-height: 1.45;
  }
</style>
