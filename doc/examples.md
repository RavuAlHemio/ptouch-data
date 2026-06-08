# ptouch-data examples

## export 9mm label from SVG for P-Touch PT-P950NW

(assumes the SVG file is already set to 9mm height and the required width)

```sh
inkscape --export-filename=label_plastic_boxes.png label_plastic_boxes.svg --export-png-color-mode=Gray_1 --export-png-antialias=0 --export-dpi=360
ptouch-encode -c -H -C -w 9 -x 560 label_plastic_boxes.png label_plastic_boxes.pt
lp -d labelprinter label_plastic_boxes.pt
```

`-x 560` extends the print data width to 560px (centering the label data), which seems to be the native value for the PT-P950NW
