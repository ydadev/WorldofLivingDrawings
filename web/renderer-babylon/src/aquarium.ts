import { Color3 } from '@babylonjs/core/Maths/math.color';
import { Vector3 } from '@babylonjs/core/Maths/math.vector';
import { Texture } from '@babylonjs/core/Materials/Textures/texture';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import type { Scene } from '@babylonjs/core/scene';

// The fixed camera looks through several real depth layers. The painted backdrop
// is deliberately quiet in the middle, so children's fish remain the focus.
export function addAquarium(scene: Scene, width: number, height: number): void {
  const w = 1024, h = 576;
  const source = document.createElement('canvas');
  source.width = w;
  source.height = h;
  const context = source.getContext('2d');
  if (!context) throw new Error('AQUARIUM_CANVAS_UNAVAILABLE');
  const water = context.createLinearGradient(0, 0, 0, h);
  water.addColorStop(0, '#104b66');
  water.addColorStop(.35, '#0b3a57');
  water.addColorStop(.78, '#082c43');
  water.addColorStop(1, '#092535');
  context.fillStyle = water;
  context.fillRect(0, 0, w, h);
  const glow = context.createRadialGradient(360, -20, 25, 360, -20, 700);
  glow.addColorStop(0, 'rgba(102,208,211,.28)');
  glow.addColorStop(1, 'rgba(102,208,211,0)');
  context.fillStyle = glow;
  context.fillRect(0, 0, w, h);
  for (let i = 0; i < 7; i++) {
    const top = 150 + i * 123;
    const beam = context.createLinearGradient(top - 85, 0, top + 130, h);
    beam.addColorStop(0, 'rgba(160,235,226,.075)');
    beam.addColorStop(1, 'rgba(160,235,226,0)');
    context.fillStyle = beam;
    context.beginPath();
    context.moveTo(top, 0);
    context.lineTo(top + 50, 0);
    context.lineTo(top + 210, h);
    context.lineTo(top + 90, h);
    context.closePath();
    context.fill();
  }
  // Distant reef and uneven sand are drawn once into a small GPU texture.
  context.fillStyle = '#16465a';
  context.beginPath();
  context.moveTo(0, 487);
  for (let x = 0; x <= w; x += 16)
    context.lineTo(x, 480 + 14 * Math.sin(x * .009) + 9 * Math.sin(x * .027));
  context.lineTo(w, h);
  context.lineTo(0, h);
  context.fill();
  context.fillStyle = '#28666a';
  context.beginPath();
  context.moveTo(0, 535);
  for (let x = 0; x <= w; x += 16)
    context.lineTo(x, 525 + 11 * Math.sin(x * .015 + 1.2) + 5 * Math.sin(x * .038));
  context.lineTo(w, h);
  context.lineTo(0, h);
  context.fill();
  for (let i = 0; i < 24; i++) {
    const x = (i * 367) % w;
    const y = 490 + (i * 19) % 58;
    context.fillStyle = i % 3 ? '#397379' : '#538b83';
    context.beginPath();
    context.ellipse(x, y, 7 + (i % 4) * 5, 3 + i % 4, -.15, 0, Math.PI * 2);
    context.fill();
  }
  const texture = new Texture(source.toDataURL('image/png'), scene, true, true);
  texture.name = 'aquarium-backdrop';
  const backdropMaterial = new StandardMaterial('aquarium-backdrop-material', scene);
  backdropMaterial.emissiveTexture = texture;
  backdropMaterial.emissiveColor = Color3.Black();
  backdropMaterial.diffuseColor = Color3.Black();
  backdropMaterial.disableLighting = true;
  backdropMaterial.specularColor = Color3.Black();
  backdropMaterial.backFaceCulling = false;
  const backdrop = MeshBuilder.CreatePlane('aquarium-backdrop-plane', { width, height }, scene);
  backdrop.position.z = 3;
  backdrop.material = backdropMaterial;
  backdrop.isPickable = false;

  const stone = new StandardMaterial('aquarium-stone', scene);
  stone.diffuseColor = new Color3(.08, .25, .31);
  stone.specularColor = Color3.Black();
  for (const [x, y, sx, sy, z] of [
    [-7.3, -3.65, 1.15, .5, 2], [-6.4, -3.85, .9, .35, 1.3],
    [7.2, -3.7, 1.1, .55, 2], [6.5, -3.9, .8, .32, .9],
  ]) {
    const rock = MeshBuilder.CreateSphere(`aquarium-rock-${x}`, { diameter: 1, segments: 12 }, scene);
    rock.position.set(x, y, z);
    rock.scaling.set(sx, sy, .4);
    rock.material = stone;
    rock.isPickable = false;
  }
  const kelp = new StandardMaterial('aquarium-kelp', scene);
  kelp.diffuseColor = new Color3(.12, .48, .43);
  kelp.specularColor = Color3.Black();
  for (let i = 0; i < 7; i++) {
    const side = i < 4 ? -1 : 1;
    const x = side * (6.4 + (i % 4) * .28);
    const z = i % 3 === 0 ? -.8 : 1.45;
    const heightKelp = 1.3 + (i * 7 % 5) * .32;
    const path = Array.from({ length: 8 }, (_, step) => {
      const t = step / 7;
      return new Vector3(x + .13 * Math.sin(t * 5 + i) * t, -4.05 + t * heightKelp, z);
    });
    const stem = MeshBuilder.CreateTube(`aquarium-kelp-${i}`, { path, radius: .025, tessellation: 5 }, scene);
    stem.material = kelp;
    stem.isPickable = false;
    for (let leaf = 1; leaf <= 4; leaf++) {
      const t = leaf / 5;
      const direction = leaf % 2 ? -1 : 1;
      const leafMesh = MeshBuilder.CreateSphere(`aquarium-leaf-${i}-${leaf}`, { diameter: 1, segments: 6 }, scene);
      leafMesh.position.set(path[Math.round(t * 7)].x + direction * .13,
        -4.05 + t * heightKelp, z);
      leafMesh.scaling.set(.22, .055, .045);
      leafMesh.rotation.z = direction * .5;
      leafMesh.material = kelp;
      leafMesh.isPickable = false;
    }
  }
}
