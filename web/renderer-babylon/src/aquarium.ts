import { Color3 } from '@babylonjs/core/Maths/math.color';
import { Texture } from '@babylonjs/core/Materials/Textures/texture';
import { StandardMaterial } from '@babylonjs/core/Materials/standardMaterial';
import { MeshBuilder } from '@babylonjs/core/Meshes/meshBuilder';
import type { Scene } from '@babylonjs/core/scene';

// The fixed camera looks into an illustrated distant reef. The middle stays
// open and calm so children's painted fish remain the focus.
export function addAquarium(scene: Scene, width: number, height: number): void {
  // Keep the centre quiet for children's paint while the distant reef
  // establishes scale. Vite emits this original artwork with the renderer.
  const texture = new Texture(new URL('./aquarium-backdrop-v2.png', import.meta.url).href,
    scene, true, true);
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
}
