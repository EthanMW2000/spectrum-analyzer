-- ~/.config/wireplumber/main.lua.d/51-clearclick-no-pipewire.lua
-- The display reads the ClearClick through ALSA directly, so keep PipeWire
-- from opening it (or changing its capture gain).
table.insert(alsa_monitor.rules, {
  matches = { { { "device.name", "matches", "alsa_card.usb-*ClearClick*" } } },
  apply_properties = { ["device.disabled"] = true },
})
