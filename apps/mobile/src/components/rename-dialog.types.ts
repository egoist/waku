export type RenameDialogProps = {
  visible: boolean;
  initialValue: string;
  onDismiss: () => void;
  onSubmit: (title: string) => Promise<void>;
};
