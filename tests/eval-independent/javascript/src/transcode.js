// Default export: the single function player.js imports.
export default function transcode(track, gain) {
  const level = normalize(gain); // @eval normalize=normalize@src/transcode.js
  const probe = track.codec.charCodeAt(0); // @eval charCodeAt=-
  return { track, level, probe };
}

function normalize(gain) {
  if (gain > 2) return 2;
  if (gain < 0) return 0;
  return gain;
}
