import { ShaderMaterial } from '@babylonjs/core/Materials/shaderMaterial';
import type { Texture } from '@babylonjs/core/Materials/Textures/texture';
import type { Scene } from '@babylonjs/core/scene';

// COLOR_0 carries the same baked, symmetric shading on both painted sides.
// A small shader keeps 100 unique child-painted textures affordable on LOW.
const vertexSource = `
precision highp float;
attribute vec3 position;
attribute vec2 uv;
attribute vec4 color;
uniform mat4 worldViewProjection;
varying vec2 vPaintUv;
varying vec3 vShade;
void main(void) {
  vPaintUv = uv;
  vShade = color.rgb;
  gl_Position = worldViewProjection * vec4(position, 1.0);
}`;

const fragmentSource = `
precision mediump float;
varying vec2 vPaintUv;
varying vec3 vShade;
uniform sampler2D paintTexture;
void main(void) {
  vec4 paint = texture2D(paintTexture, vPaintUv);
  gl_FragColor = vec4(paint.rgb * vShade, paint.a);
}`;

export function createFishPaintMaterial(scene: Scene, name: string, texture: Texture): ShaderMaterial {
  const material = new ShaderMaterial(name, scene, { vertexSource, fragmentSource }, {
    attributes: ['position', 'uv', 'color'],
    uniforms: ['worldViewProjection'],
    samplers: ['paintTexture'],
  });
  material.setTexture('paintTexture', texture);
  return material;
}
