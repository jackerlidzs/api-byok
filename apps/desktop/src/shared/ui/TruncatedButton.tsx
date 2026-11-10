import { useRef, useState, type ComponentProps } from "react";
import { Button } from "./Button";
import { TooltipTrigger } from "./TooltipTrigger";
import styles from "./TruncatedButton.module.scss";

/**
 * A button that shows its full text in a tooltip only when the text is truncated.
 * The button is a flex container, so the ellipsis applies to the inner text span;
 * truncation is measured on hover/focus because fonts may not be loaded at mount time.
 */
export function TruncatedButton({ label, ...props }: ComponentProps<typeof Button> & { label: string }) {
  const element = useRef<HTMLSpanElement>(null);
  const [truncated, setTruncated] = useState(false);
  const measure = () => {
    const text = element.current;
    if (text) setTruncated(text.scrollWidth > text.clientWidth);
  };
  const button = <Button {...props} onPointerEnter={measure} onFocus={measure}>
    <span ref={element} className={styles.label}>{label}</span>
  </Button>;
  return truncated ? <TooltipTrigger label={label}>{button}</TooltipTrigger> : button;
}
