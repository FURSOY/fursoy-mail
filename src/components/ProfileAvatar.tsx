import { useEffect, useRef, useState, type CSSProperties } from "react";
import { proxiedProfileImageUrl } from "../profileImage";

interface ProfileAvatarProps {
  picture?: string | null;
  email: string;
  className: string;
  fallbackClassName?: string;
  fallbackStyle?: CSSProperties;
  alt?: string;
}

export function ProfileAvatar({
  picture,
  email,
  className,
  fallbackClassName = "bg-zinc-700 text-zinc-300",
  fallbackStyle,
  alt = "",
}: ProfileAvatarProps) {
  const [attempt, setAttempt] = useState(0);
  const [failed, setFailed] = useState(false);
  const retryTimerRef = useRef<number | null>(null);
  const source = proxiedProfileImageUrl(picture, attempt);

  useEffect(() => {
    setAttempt(0);
    setFailed(false);
    return () => {
      if (retryTimerRef.current !== null) window.clearTimeout(retryTimerRef.current);
    };
  }, [picture]);

  const handleError = () => {
    setFailed(true);
    if (attempt > 0 || retryTimerRef.current !== null) return;
    retryTimerRef.current = window.setTimeout(() => {
      retryTimerRef.current = null;
      setAttempt(1);
      setFailed(false);
    }, 1_000);
  };

  if (!source || failed) {
    return (
      <div
        className={`${className} flex items-center justify-center font-bold ${fallbackClassName}`}
        style={fallbackStyle}
        role={alt ? "img" : undefined}
        aria-hidden={alt ? undefined : true}
        aria-label={alt || undefined}
      >
        {email[0]?.toUpperCase() ?? "?"}
      </div>
    );
  }

  return <img src={source} alt={alt} className={className} onError={handleError} />;
}
