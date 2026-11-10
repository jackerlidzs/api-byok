import controls from "./Controls.module.scss";
import { Icon } from "./Icon";
import { chevronDoubleLeftIcon, chevronDoubleRightIcon, chevronLeftIcon, chevronRightIcon } from "./icons";
import { Select } from "./Select";
import { TooltipTrigger } from "./TooltipTrigger";
import styles from "./Pagination.module.scss";

export function Pagination({ page, pageCount, pageSize, total, pageSizes = [20, 50, 100], onPageChange, onPageSizeChange }: {
  page: number;
  pageCount: number;
  pageSize: number;
  total: number;
  pageSizes?: number[];
  onPageChange: (page: number) => void;
  onPageSizeChange: (pageSize: number) => void;
}) {
  const disabledPrevious = page <= 1;
  const disabledNext = page >= pageCount;
  return <div className={styles.root}>
    <span>{t("{count} total", { count: total })}</span>
    <div className={styles.controls}>
      <div className={styles.pageSize}><Select ariaLabel={t("Items per page")} value={String(pageSize)} options={pageSizes.map((size) => ({ value: String(size), label: t("{count} per page", { count: size }) }))} onChange={(value) => onPageSizeChange(Number(value))} /></div>
      <span>{t("Page {page} of {count}", { page, count: pageCount })}</span>
      <TooltipTrigger label={t("First page")}><button className={controls.iconButton} aria-label={t("First page")} disabled={disabledPrevious} onClick={() => onPageChange(1)}><Icon icon={chevronDoubleLeftIcon} size="1.1em" /></button></TooltipTrigger>
      <TooltipTrigger label={t("Previous page")}><button className={controls.iconButton} aria-label={t("Previous page")} disabled={disabledPrevious} onClick={() => onPageChange(page - 1)}><Icon icon={chevronLeftIcon} size="1.1em" /></button></TooltipTrigger>
      <TooltipTrigger label={t("Next page")}><button className={controls.iconButton} aria-label={t("Next page")} disabled={disabledNext} onClick={() => onPageChange(page + 1)}><Icon icon={chevronRightIcon} size="1.1em" /></button></TooltipTrigger>
      <TooltipTrigger label={t("Last page")}><button className={controls.iconButton} aria-label={t("Last page")} disabled={disabledNext} onClick={() => onPageChange(pageCount)}><Icon icon={chevronDoubleRightIcon} size="1.1em" /></button></TooltipTrigger>
    </div>
  </div>;
}
