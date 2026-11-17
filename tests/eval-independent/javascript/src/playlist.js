export class Playlist {
  constructor() {
    this.tracks = [];
  }

  add(track) {
    this.tracks.push(track);
    return this.tracks.length;
  }

  totalSeconds() {
    let sum = 0;
    for (const t of this.tracks) {
      sum += toSeconds(t.length); // @eval toSeconds=toSeconds@src/playlist.js
    }
    return sum;
  }
}

function toSeconds(length) {
  const [m, s] = String(length).split(':').map(Number);
  return m * 60 + s;
}
