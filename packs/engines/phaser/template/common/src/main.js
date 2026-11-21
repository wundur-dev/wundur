import * as Phaser from 'phaser';

// A first scene: a square you move with the arrow keys. Replace it with your game.
class Play extends Phaser.Scene {
  constructor() {
    super('play');
  }

  create() {
    // A texture drawn in code, so the starter needs no image files.
    const g = this.make.graphics({ x: 0, y: 0 }, false);
    g.fillStyle(0x66ccff).fillRect(0, 0, 32, 32);
    g.generateTexture('player', 32, 32);
    g.destroy();

    this.player = this.physics.add.sprite(400, 300, 'player');
    this.player.setCollideWorldBounds(true);
    this.cursors = this.input.keyboard.createCursorKeys();
    this.add.text(16, 16, `${document.title}: arrow keys to move`, { fontFamily: 'sans-serif', fontSize: '18px', color: '#ffffff' });
  }

  update() {
    const speed = 220;
    const { left, right, up, down } = this.cursors;
    this.player.setVelocity(
      (right.isDown ? speed : 0) - (left.isDown ? speed : 0),
      (down.isDown ? speed : 0) - (up.isDown ? speed : 0),
    );
  }
}

new Phaser.Game({
  type: Phaser.AUTO,
  parent: 'game',
  width: 800,
  height: 600,
  backgroundColor: '#10141c',
  pixelArt: true,
  physics: { default: 'arcade' },
  scale: { mode: Phaser.Scale.FIT, autoCenter: Phaser.Scale.CENTER_BOTH },
  scene: [Play],
});
