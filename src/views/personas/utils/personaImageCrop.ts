export type PersonaCropTargetKey = 'portrait' | 'avatar';

export interface PersonaCropTarget {
  key: PersonaCropTargetKey;
  label: string;
  width: number;
  height: number;
}

export interface PersonaCropSelection {
  centerX: number;
  centerY: number;
  zoom: number;
}

export interface PersonaCropRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export const PERSONA_CROP_TARGETS: Record<PersonaCropTargetKey, PersonaCropTarget> = {
  portrait: { key: 'portrait', label: '立绘', width: 900, height: 1200 },
  avatar: { key: 'avatar', label: '头像', width: 768, height: 768 }
};

export const PERSONA_CROP_ZOOM_MIN = 1;
export const PERSONA_CROP_ZOOM_MAX = 3;

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function resolveBaseCropSize(
  sourceWidth: number,
  sourceHeight: number,
  target: PersonaCropTarget
) {
  const sourceRatio = sourceWidth / sourceHeight;
  const targetRatio = target.width / target.height;
  if (sourceRatio > targetRatio) {
    return { width: sourceHeight * targetRatio, height: sourceHeight };
  }
  return { width: sourceWidth, height: sourceWidth / targetRatio };
}

export function createInitialPersonaCropSelection(
  sourceWidth: number,
  sourceHeight: number
): PersonaCropSelection {
  return {
    centerX: sourceWidth / 2,
    centerY: sourceHeight / 2,
    zoom: PERSONA_CROP_ZOOM_MIN
  };
}

export function resolvePersonaCropRect(
  sourceWidth: number,
  sourceHeight: number,
  target: PersonaCropTarget,
  selection: PersonaCropSelection
): PersonaCropRect {
  const zoom = clamp(selection.zoom, PERSONA_CROP_ZOOM_MIN, PERSONA_CROP_ZOOM_MAX);
  const base = resolveBaseCropSize(sourceWidth, sourceHeight, target);
  const width = base.width / zoom;
  const height = base.height / zoom;
  const centerX = clamp(selection.centerX, width / 2, sourceWidth - width / 2);
  const centerY = clamp(selection.centerY, height / 2, sourceHeight - height / 2);
  return {
    x: centerX - width / 2,
    y: centerY - height / 2,
    width,
    height
  };
}

export function clampPersonaCropSelection(
  sourceWidth: number,
  sourceHeight: number,
  target: PersonaCropTarget,
  selection: PersonaCropSelection
): PersonaCropSelection {
  const zoom = clamp(selection.zoom, PERSONA_CROP_ZOOM_MIN, PERSONA_CROP_ZOOM_MAX);
  const rect = resolvePersonaCropRect(sourceWidth, sourceHeight, target, {
    ...selection,
    zoom
  });
  return {
    centerX: rect.x + rect.width / 2,
    centerY: rect.y + rect.height / 2,
    zoom
  };
}

export function panPersonaCropSelection(
  sourceWidth: number,
  sourceHeight: number,
  target: PersonaCropTarget,
  selection: PersonaCropSelection,
  imageDeltaX: number,
  imageDeltaY: number,
  viewportWidth: number,
  viewportHeight: number
): PersonaCropSelection {
  const rect = resolvePersonaCropRect(sourceWidth, sourceHeight, target, selection);
  return clampPersonaCropSelection(sourceWidth, sourceHeight, target, {
    ...selection,
    // 用户拖动的是图片本身，因此图片向右移动时裁剪中心应向左移动。
    centerX: selection.centerX - imageDeltaX * (rect.width / Math.max(viewportWidth, 1)),
    centerY: selection.centerY - imageDeltaY * (rect.height / Math.max(viewportHeight, 1))
  });
}

function canvasToBlob(canvas: HTMLCanvasElement, type: string): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob(
      (blob) => {
        if (blob) resolve(blob);
        else reject(new Error('当前环境无法生成裁剪图片。'));
      },
      type,
      0.92
    );
  });
}

async function decodeImage(file: File): Promise<{
  source: CanvasImageSource;
  width: number;
  height: number;
  dispose: () => void;
}> {
  if (typeof createImageBitmap === 'function') {
    const bitmap = await createImageBitmap(file);
    return {
      source: bitmap,
      width: bitmap.width,
      height: bitmap.height,
      dispose: () => bitmap.close()
    };
  }

  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    return {
      source: image,
      width: image.naturalWidth,
      height: image.naturalHeight,
      dispose: () => URL.revokeObjectURL(url)
    };
  } catch (error) {
    URL.revokeObjectURL(url);
    throw error;
  }
}

function outputExtension(type: string): string {
  if (type === 'image/png') return 'png';
  if (type === 'image/jpeg') return 'jpg';
  return 'webp';
}

export async function createPersonaImageCrops(
  file: File,
  selections: Record<PersonaCropTargetKey, PersonaCropSelection>
): Promise<Record<PersonaCropTargetKey, File>> {
  const decoded = await decodeImage(file);
  const outputType = ['image/png', 'image/jpeg', 'image/webp'].includes(file.type)
    ? file.type
    : 'image/png';
  const baseName = file.name.replace(/\.[^.]+$/u, '') || 'persona-image';

  try {
    const entries = await Promise.all(
      (Object.keys(PERSONA_CROP_TARGETS) as PersonaCropTargetKey[]).map(async (key) => {
        const target = PERSONA_CROP_TARGETS[key];
        const rect = resolvePersonaCropRect(
          decoded.width,
          decoded.height,
          target,
          selections[key]
        );
        const canvas = document.createElement('canvas');
        canvas.width = target.width;
        canvas.height = target.height;
        const context = canvas.getContext('2d');
        if (!context) throw new Error('当前环境无法创建图片裁剪画布。');
        context.drawImage(
          decoded.source,
          rect.x,
          rect.y,
          rect.width,
          rect.height,
          0,
          0,
          target.width,
          target.height
        );
        const blob = await canvasToBlob(canvas, outputType);
        return [
          key,
          new File([blob], `${baseName}-${key}.${outputExtension(blob.type)}`, {
            type: blob.type
          })
        ] as const;
      })
    );
    return Object.fromEntries(entries) as Record<PersonaCropTargetKey, File>;
  } finally {
    decoded.dispose();
  }
}
