import {Component, EventEmitter, Input, Output} from '@angular/core';
import {FormsModule} from '@angular/forms';
import {DiagnosticsSnapshot, emptyDiagnostics} from './continuity-model';
@Component({selector:'app-continuity-panel',imports:[FormsModule],templateUrl:'./continuity-panel.html',
  styles:[`:host{display:block;margin-bottom:24px;padding:24px;border:1px solid #344052;border-radius:16px}table{width:100%;border-collapse:collapse}th,td{text-align:left;padding:8px;border-bottom:1px solid #344052}p,li{overflow-wrap:anywhere}button{padding:10px;margin-right:12px}input{margin-right:8px}.warning{color:#ecad75}li{margin:8px 0}`]})
export class ContinuityPanel {
  @Input() data:DiagnosticsSnapshot=emptyDiagnostics();
  @Input() disabled=true;
  @Output() captureRequested=new EventEmitter<boolean>();
  @Output() enabledChange=new EventEmitter<boolean>();
}
