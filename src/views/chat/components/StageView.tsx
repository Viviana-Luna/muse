import { useEffect, useState } from 'react';
import type { RefObject } from 'react';

interface StageViewProps {
  stageStateClass: string;
  backgroundPath: string;
  portraitPath: string;
  personaName?: string;
  showPortrait?: boolean;
  visualizerCanvasRef?: RefObject<HTMLCanvasElement | null>;
}

export function StageView({
  stageStateClass,
  backgroundPath,
  portraitPath,
  personaName,
  showPortrait = true,
  visualizerCanvasRef
}: StageViewProps) {
  const [backgroundFailed, setBackgroundFailed] = useState(false);
  const [portraitFailed, setPortraitFailed] = useState(false);

  useEffect(() => setBackgroundFailed(false), [backgroundPath]);
  useEffect(() => setPortraitFailed(false), [portraitPath]);

  return (
    <section className={`stage ${stageStateClass}`} aria-label="角色舞台">
      {backgroundPath && !backgroundFailed && (
        <img
          className="stage-background"
          src={backgroundPath}
          alt=""
          draggable={false}
          onError={() => setBackgroundFailed(true)}
        />
      )}
      <div className="stage-vignette" />
      {showPortrait && portraitPath && !portraitFailed && (
        <div className="portrait-wrap">
          <div className="portrait-frame">
            <img
              className="portrait"
              src={portraitPath}
              alt={personaName || '角色立绘'}
              draggable={false}
              onError={() => setPortraitFailed(true)}
            />
          </div>
        </div>
      )}
    </section>
  );
}
