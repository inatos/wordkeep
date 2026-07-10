# Integration hooks

Curated cross-subsystem wiring for `integration_hooks`. Copy to your repo as
`.wordkeep/integration-hooks.md` and edit for your project.

## render -> physics

`render_submit` calls `physics_step` during the frame loop.

## audio -> gameplay

`audio_mix` reads emitter state that `gameplay_tick` updates each frame.
