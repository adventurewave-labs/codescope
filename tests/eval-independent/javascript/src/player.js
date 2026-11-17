// Media player core. playlist.js and transcode.js are imported below.
import { Playlist } from './playlist.js';
import transcode from './transcode.js';

export class Player {
  constructor(container) {
    this.container = container;
    this.playlist = new Playlist();
  }

  play(track) {
    this.playlist.add(track); // @eval add=Playlist::add@src/playlist.js
    const gain = Math.max(0, track.gain || 1); // @eval max=-
    const stream = transcode(track, gain); // @eval transcode=transcode@src/transcode.js
    return stream;
  }

  seek(seconds) {
    return Math.min(seconds, this.duration()); // @eval min=-
  }

  duration() {
    return this.playlist.totalSeconds(); // @eval totalSeconds=Playlist::totalSeconds@src/playlist.js
  }
}
