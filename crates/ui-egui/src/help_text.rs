//! What each tool and the most-used commands do, in one plain sentence (English, Spanish,
//! Portuguese; other languages read the English): shown under each command palette result, in
//! the long-press card of the simple interface's bars and sheet, and searched by the palette
//! ("borrar texto" finds Remove with AI). Families of tools that do similar jobs carry a "Which
//! one should I use?" hint ([`WHICH`]).
//!
//! One entry per line (`xtask tool-help` checks every tool has its three sentences).

use crate::i18n::Lang;

/// What `id` (`tool:Name` or a command id) does, in three languages.
pub struct Help {
    pub id: &'static str,
    pub en: &'static str,
    pub es: &'static str,
    pub pt: &'static str,
}

impl Help {
    /// The sentence in `lang` (English for languages without one).
    pub fn text(&self, lang: Lang) -> &'static str {
        match lang_key(lang) {
            "es" => self.es,
            "pt" => self.pt,
            _ => self.en,
        }
    }
}

/// "es", "pt" or "en": the languages the sentences are written in.
fn lang_key(lang: Lang) -> &'static str {
    let c = lang.code();
    if c.starts_with("es") {
        "es"
    } else if c.starts_with("pt") {
        "pt"
    } else {
        "en"
    }
}

const fn h(id: &'static str, en: &'static str, es: &'static str, pt: &'static str) -> Help {
    Help { id, en, es, pt }
}

#[rustfmt::skip]
pub const HELP: &[Help] = &[
    // ---------------------------------------------------------------- tools
    h("tool:Move", "Drag layers or the selected pixels to move them", "Arrastra capas o lo seleccionado para moverlo", "Arraste camadas ou a seleção para movê-las"),
    h("tool:Artboard", "Make and size artboards: several pages in one document", "Crea y ajusta mesas de trabajo: varias páginas en un documento", "Crie e ajuste pranchetas: várias páginas em um documento"),
    h("tool:RectMarquee", "Drag a rectangle to select an area", "Arrastra un rectángulo para seleccionar una zona", "Arraste um retângulo para selecionar uma área"),
    h("tool:EllipseMarquee", "Drag an ellipse or a circle to select", "Arrastra una elipse o un círculo para seleccionar", "Arraste uma elipse ou um círculo para selecionar"),
    h("tool:SingleRowMarquee", "Select one row of pixels from side to side", "Selecciona una fila de píxeles de lado a lado", "Seleciona uma linha de pixels de lado a lado"),
    h("tool:SingleColumnMarquee", "Select one column of pixels from top to bottom", "Selecciona una columna de píxeles de arriba abajo", "Seleciona uma coluna de pixels de cima a baixo"),
    h("tool:Lasso", "Draw freehand around what you want to select", "Dibuja a mano alzada alrededor de lo que quieres seleccionar", "Desenhe à mão livre ao redor do que quer selecionar"),
    h("tool:PolygonLasso", "Tap corner by corner to select with straight edges", "Toca esquina por esquina para seleccionar con bordes rectos", "Toque canto por canto para selecionar com bordas retas"),
    h("tool:MagneticLasso", "Trace along an edge and the selection sticks to it", "Recorre un borde y la selección se pega a él", "Percorra uma borda e a seleção gruda nela"),
    h("tool:MagicWand", "Tap a color to select the similar pixels around it", "Toca un color para seleccionar los píxeles parecidos de alrededor", "Toque uma cor para selecionar os pixels parecidos ao redor"),
    h("tool:Crop", "Cut the image down to the frame you draw", "Recorta la imagen al marco que dibujes", "Corta a imagem no quadro que você desenhar"),
    h("tool:PerspectiveCrop", "Crop and straighten a tilted page or sign", "Recorta y endereza una página o un cartel inclinado", "Corta e endireita uma página ou placa inclinada"),
    h("tool:Eyedropper", "Tap the image to pick up its color", "Toca la imagen para tomar su color", "Toque a imagem para pegar a cor"),
    h("tool:ColorSampler", "Pin points that show their color values in Info", "Fija puntos que muestran sus valores de color en Información", "Fixe pontos que mostram os valores de cor em Informações"),
    h("tool:Ruler", "Measure distances and angles; it can straighten the image", "Mide distancias y ángulos; también endereza la imagen", "Mede distâncias e ângulos; também endireita a imagem"),
    h("tool:Note", "Leave text notes on the document", "Deja notas de texto sobre el documento", "Deixe notas de texto no documento"),
    h("tool:Count", "Tap to number and count things in the image", "Toca para numerar y contar cosas en la imagen", "Toque para numerar e contar coisas na imagem"),
    h("tool:Brush", "Paint soft or hard strokes in the foreground color", "Pinta trazos suaves o duros con el color frontal", "Pinte traços suaves ou duros com a cor principal"),
    h("tool:Pencil", "Paint hard-edged lines, pixel by pixel", "Pinta líneas de borde duro, píxel a píxel", "Pinte linhas de borda dura, pixel a pixel"),
    h("tool:MixerBrush", "Paint that mixes with the colors already there, like wet paint", "Pinta mezclando con los colores que ya hay, como pintura fresca", "Pinta misturando com as cores que já existem, como tinta fresca"),
    h("tool:ColorReplacement", "Paint over a color to change it and keep its shading", "Pinta sobre un color para cambiarlo y conservar sus sombras", "Pinte sobre uma cor para trocá-la e manter o sombreado"),
    h("tool:Eraser", "Erase as you paint (to transparency, or to the background color)", "Borra al pintar (a transparente o al color de fondo)", "Apaga ao pintar (para transparente ou para a cor de fundo)"),
    h("tool:BackgroundEraser", "Erase the background along an edge and keep the subject", "Borra el fondo a lo largo de un borde y conserva el sujeto", "Apaga o fundo ao longo de uma borda e mantém o objeto"),
    h("tool:MagicEraser", "Tap a color to erase it and the similar pixels around it", "Toca un color para borrarlo junto con los parecidos de alrededor", "Toque uma cor para apagá-la junto com as parecidas ao redor"),
    h("tool:Gradient", "Drag to fill with a smooth blend of colors", "Arrastra para rellenar con una transición suave de colores", "Arraste para preencher com uma transição suave de cores"),
    h("tool:PaintBucket", "Tap to fill an area of similar color", "Toca para rellenar una zona de color parecido", "Toque para preencher uma área de cor parecida"),
    h("tool:Type", "Tap to write horizontal text; drag for a text box", "Toca para escribir texto horizontal; arrastra para una caja de texto", "Toque para escrever texto horizontal; arraste para uma caixa de texto"),
    h("tool:VerticalType", "Write text from top to bottom (Japanese, sound effects)", "Escribe texto de arriba abajo (japonés, onomatopeyas)", "Escreva texto de cima para baixo (japonês, onomatopeias)"),
    h("tool:HorizontalTypeMask", "Type letters that become a selection", "Escribe letras que se convierten en una selección", "Digite letras que viram uma seleção"),
    h("tool:VerticalTypeMask", "Type vertical letters that become a selection", "Escribe letras verticales que se convierten en una selección", "Digite letras verticais que viram uma seleção"),
    h("tool:BubbleType", "Tap a speech bubble and the text fits inside it", "Toca un globo y el texto se ajusta dentro", "Toque um balão e o texto se ajusta dentro dele"),
    h("tool:Hand", "Drag to move around the image", "Arrastra para moverte por la imagen", "Arraste para se mover pela imagem"),
    h("tool:RotateView", "Turn the view of the canvas without changing the image", "Gira la vista del lienzo sin cambiar la imagen", "Gira a visualização da tela sem alterar a imagem"),
    h("tool:Zoom", "Tap or drag to zoom in and out", "Toca o arrastra para acercar y alejar", "Toque ou arraste para aproximar e afastar"),
    h("tool:Remove", "Erase by painting over it, without AI (faster, less precise)", "Borra pintando, sin IA (más rápido, menos preciso)", "Apaga pintando, sem IA (mais rápido, menos preciso)"),
    h("tool:RemoveAi", "Paint over text or objects and the AI erases them, filling in the background", "Pinta sobre texto u objetos y la IA los borra rellenando el fondo", "Pinte sobre texto ou objetos e a IA os apaga preenchendo o fundo"),
    h("tool:SpotHealing", "Tap a small spot and it blends with what is around it", "Toca una mancha pequeña y se mezcla con lo de alrededor", "Toque uma mancha pequena e ela se mistura com o redor"),
    h("tool:Healing", "Pick a source with Alt, then paint to repair with its texture", "Elige un origen con Alt y pinta para reparar con esa textura", "Escolha uma origem com Alt e pinte para reparar com essa textura"),
    h("tool:Patch", "Circle an area and drag it onto clean texture to repair it", "Rodea una zona y arrástrala sobre textura limpia para repararla", "Contorne uma área e arraste sobre textura limpa para repará-la"),
    h("tool:ContentAwareMove", "Select something and drag it; the hole it leaves fills itself", "Selecciona algo y arrástralo; el hueco que deja se rellena solo", "Selecione algo e arraste; o buraco que fica se preenche sozinho"),
    h("tool:RedEye", "Tap a red eye in a photo to fix it", "Toca un ojo rojo en una foto para corregirlo", "Toque um olho vermelho numa foto para corrigi-lo"),
    h("tool:CloneStamp", "Copy pixels from one place and paint them in another", "Copia píxeles de un sitio y los pinta en otro", "Copia pixels de um lugar e pinta em outro"),
    h("tool:PatternStamp", "Paint with a repeating pattern", "Pinta con un patrón que se repite", "Pinte com um padrão que se repete"),
    h("tool:HistoryBrush", "Paint back an earlier state of the image", "Pinta para recuperar un estado anterior de la imagen", "Pinte para recuperar um estado anterior da imagem"),
    h("tool:ArtHistoryBrush", "Paint an earlier state back in stylized dabs", "Pinta un estado anterior con trazos estilizados", "Pinte um estado anterior com traços estilizados"),
    h("tool:Blur", "Paint to soften and blur details", "Pinta para suavizar y desenfocar detalles", "Pinte para suavizar e desfocar detalhes"),
    h("tool:Sharpen", "Paint to make details crisper", "Pinta para dar más nitidez a los detalles", "Pinte para deixar os detalhes mais nítidos"),
    h("tool:Smudge", "Drag to smear pixels like a finger in wet paint", "Arrastra para difuminar como un dedo en pintura fresca", "Arraste para borrar como um dedo em tinta fresca"),
    h("tool:Dodge", "Paint to lighten areas", "Pinta para aclarar zonas", "Pinte para clarear áreas"),
    h("tool:Burn", "Paint to darken areas", "Pinta para oscurecer zonas", "Pinte para escurecer áreas"),
    h("tool:Sponge", "Paint to make colors more or less saturated", "Pinta para saturar o desaturar los colores", "Pinte para saturar ou dessaturar as cores"),
    h("tool:QuickSelection", "Paint over an object and the selection grows to its edges", "Pinta sobre un objeto y la selección crece hasta sus bordes", "Pinte sobre um objeto e a seleção cresce até as bordas"),
    h("tool:ObjectSelection", "Draw a box around an object and it is selected for you", "Dibuja un recuadro alrededor de un objeto y se selecciona solo", "Desenhe um retângulo ao redor de um objeto e ele é selecionado sozinho"),
    h("tool:Pen", "Click points to draw precise paths and curves", "Haz clic en puntos para trazar líneas y curvas precisas", "Clique em pontos para traçar caminhos e curvas precisos"),
    h("tool:FreeformPen", "Draw a path freehand", "Dibuja un trazado a mano alzada", "Desenhe um traçado à mão livre"),
    h("tool:CurvaturePen", "Click points and smooth curves form between them", "Haz clic en puntos y se forman curvas suaves entre ellos", "Clique em pontos e curvas suaves se formam entre eles"),
    h("tool:AddAnchorPoint", "Click a path to add a point to it", "Haz clic en un trazado para añadirle un punto", "Clique num traçado para adicionar um ponto"),
    h("tool:DeleteAnchorPoint", "Click a point to take it out of its path", "Haz clic en un punto para quitarlo del trazado", "Clique num ponto para tirá-lo do traçado"),
    h("tool:ConvertPoint", "Turn a point into a sharp corner or a smooth curve", "Convierte un punto en esquina o en curva suave", "Converte um ponto em canto ou em curva suave"),
    h("tool:PathSelection", "Select and move whole paths and shapes", "Selecciona y mueve trazados y formas enteros", "Seleciona e move traçados e formas inteiros"),
    h("tool:DirectSelection", "Select and move single points of a path", "Selecciona y mueve puntos sueltos de un trazado", "Seleciona e move pontos soltos de um traçado"),
    h("tool:Rectangle", "Draw rectangles and rounded boxes as editable shapes", "Dibuja rectángulos y cajas redondeadas como formas editables", "Desenhe retângulos e caixas arredondadas como formas editáveis"),
    h("tool:EllipseShape", "Draw ellipses and circles as editable shapes", "Dibuja elipses y círculos como formas editables", "Desenhe elipses e círculos como formas editáveis"),
    h("tool:Triangle", "Draw triangles as editable shapes", "Dibuja triángulos como formas editables", "Desenhe triângulos como formas editáveis"),
    h("tool:Polygon", "Draw polygons and stars as editable shapes", "Dibuja polígonos y estrellas como formas editables", "Desenhe polígonos e estrelas como formas editáveis"),
    h("tool:Line", "Draw straight lines and arrows", "Dibuja líneas rectas y flechas", "Desenhe linhas retas e setas"),
    h("tool:CustomShape", "Draw a shape from the library (hearts, arrows, bubbles…)", "Dibuja una forma de la biblioteca (corazones, flechas, globos…)", "Desenhe uma forma da biblioteca (corações, setas, balões…)"),
    h("tool:Slice", "Cut the image into pieces for the web", "Divide la imagen en sectores para la web", "Divide a imagem em fatias para a web"),
    h("tool:SliceSelect", "Select and adjust slices", "Selecciona y ajusta sectores", "Seleciona e ajusta fatias"),
    h("tool:Frame", "Draw a frame that crops the image you put inside it", "Dibuja un marco que recorta la imagen que pongas dentro", "Desenhe um quadro que recorta a imagem colocada dentro dele"),
    // ---------------------------------------------------------------- AI
    h("edit.inpaintAi", "Select text or objects and the AI erases them, rebuilding the background", "Selecciona texto u objetos y la IA los borra reconstruyendo el fondo", "Selecione texto ou objetos e a IA os apaga reconstruindo o fundo"),
    h("edit.generativeFill", "Select an area, describe what goes there and the AI draws it", "Selecciona una zona, describe qué poner y la IA lo dibuja", "Selecione uma área, descreva o que colocar e a IA desenha"),
    h("image.upscaleAi", "Make the image bigger and sharper with AI (2x or 4x)", "Agranda la imagen y la hace más nítida con IA (2x o 4x)", "Aumenta a imagem e a deixa mais nítida com IA (2x ou 4x)"),
    h("window.aiModels", "Download, update or delete the AI models on this device", "Descarga, actualiza o borra los modelos de IA de este dispositivo", "Baixe, atualize ou apague os modelos de IA deste dispositivo"),
    // ---------------------------------------------------------------- File
    h("file.new", "Start a blank document of the size you choose", "Empieza un documento en blanco del tamaño que elijas", "Comece um documento em branco do tamanho que quiser"),
    h("file.open", "Open an image or a PSD from your device", "Abre una imagen o un PSD de tu dispositivo", "Abra uma imagem ou um PSD do seu dispositivo"),
    h("file.openAs", "Open a file as a format you pick", "Abre un archivo con el formato que elijas", "Abre um arquivo no formato que você escolher"),
    h("file.save", "Save the document with its layers", "Guarda el documento con sus capas", "Salva o documento com as camadas"),
    h("file.saveAs", "Save under a new name or in another format", "Guarda con otro nombre o en otro formato", "Salva com outro nome ou em outro formato"),
    h("file.saveACopy", "Save a copy and keep working on the original", "Guarda una copia y sigue trabajando en el original", "Salva uma cópia e continua no original"),
    h("file.close", "Close this document", "Cierra este documento", "Fecha este documento"),
    h("file.closeAll", "Close every open document", "Cierra todos los documentos abiertos", "Fecha todos os documentos abertos"),
    h("file.revert", "Go back to the last saved version", "Vuelve a la última versión guardada", "Volta para a última versão salva"),
    h("file.recoverDocuments", "Bring back work that was not saved before a crash", "Recupera el trabajo sin guardar tras un cierre inesperado", "Recupera o trabalho não salvo depois de um fechamento inesperado"),
    h("file.export.quickExportAsPng", "Export the image as a PNG in one step", "Exporta la imagen como PNG en un paso", "Exporta a imagem como PNG em um passo"),
    h("file.export.exportAs", "Export as PNG, JPEG or WebP with size and quality", "Exporta como PNG, JPEG o WebP eligiendo tamaño y calidad", "Exporta como PNG, JPEG ou WebP escolhendo tamanho e qualidade"),
    h("file.export.saveForWebLegacy", "Make small web images, comparing quality and size", "Crea imágenes ligeras para la web comparando calidad y peso", "Cria imagens leves para a web comparando qualidade e tamanho"),
    h("file.export.layersToFiles", "Save every layer as its own file", "Guarda cada capa como un archivo aparte", "Salva cada camada como um arquivo separado"),
    h("file.placeEmbedded", "Put another image into this document as a new layer", "Coloca otra imagen en este documento como capa nueva", "Coloca outra imagem neste documento como camada nova"),
    h("file.automate.batch", "Run an action on a whole folder of images", "Aplica una acción a toda una carpeta de imágenes", "Aplica uma ação a uma pasta inteira de imagens"),
    h("file.automate.fitImage", "Resize the image to fit inside a width and height", "Redimensiona la imagen para que quepa en un ancho y alto", "Redimensiona a imagem para caber numa largura e altura"),
    h("file.scripts.imageProcessor", "Convert and resize many images at once", "Convierte y redimensiona muchas imágenes a la vez", "Converte e redimensiona muitas imagens de uma vez"),
    h("file.scripts.loadFilesIntoStack", "Open several files as layers of one document", "Abre varios archivos como capas de un solo documento", "Abre vários arquivos como camadas de um documento"),
    h("file.fileInfo", "See and edit the file's details (author, copyright…)", "Ve y edita los datos del archivo (autor, copyright…)", "Veja e edite os dados do arquivo (autor, copyright…)"),
    h("file.print", "Print the document", "Imprime el documento", "Imprime o documento"),
    // ---------------------------------------------------------------- Edit
    h("edit.undo", "Undo the last change", "Deshace el último cambio", "Desfaz a última alteração"),
    h("edit.redo", "Redo what you undid", "Rehace lo que deshiciste", "Refaz o que você desfez"),
    h("edit.toggleLastState", "Switch between before and after the last change", "Alterna entre antes y después del último cambio", "Alterna entre antes e depois da última alteração"),
    h("edit.fade", "Soften the last filter or paint stroke", "Atenúa el último filtro o trazo", "Atenua o último filtro ou traço"),
    h("edit.cut", "Cut the selection to the clipboard", "Corta lo seleccionado al portapapeles", "Recorta a seleção para a área de transferência"),
    h("edit.copy", "Copy the selection on this layer", "Copia lo seleccionado de esta capa", "Copia a seleção desta camada"),
    h("edit.copyMerged", "Copy what you see in the selection, all layers together", "Copia lo que se ve en la selección, todas las capas juntas", "Copia o que se vê na seleção, todas as camadas juntas"),
    h("edit.paste", "Paste what you copied as a new layer", "Pega lo copiado como una capa nueva", "Cola o que foi copiado como camada nova"),
    h("edit.pasteSpecial.pasteInPlace", "Paste exactly where it was copied from", "Pega justo donde se copió", "Cola exatamente onde foi copiado"),
    h("edit.clear", "Delete the selected pixels", "Elimina los píxeles seleccionados", "Apaga os pixels selecionados"),
    h("edit.search", "Find any command, tool or panel by name", "Busca cualquier comando, herramienta o panel por su nombre", "Procura qualquer comando, ferramenta ou painel pelo nome"),
    h("edit.checkSpelling", "Check the spelling of your text layers", "Revisa la ortografía de las capas de texto", "Verifica a ortografia das camadas de texto"),
    h("edit.findAndReplaceText", "Find words in your text layers and replace them", "Busca palabras en las capas de texto y reemplázalas", "Procura palavras nas camadas de texto e substitui"),
    h("edit.fill", "Fill the selection with a color, a pattern or content-aware", "Rellena la selección con un color, un patrón o según el contenido", "Preenche a seleção com uma cor, um padrão ou sensível ao conteúdo"),
    h("edit.stroke", "Draw a line around the selection's edge", "Dibuja una línea por el borde de la selección", "Desenha uma linha na borda da seleção"),
    h("edit.contentAwareFill", "Fill the selection with matching texture from around it, without AI", "Rellena la selección con textura de alrededor, sin IA", "Preenche a seleção com textura do redor, sem IA"),
    h("edit.contentAwareScale", "Resize without squashing people or key objects", "Cambia el tamaño sin aplastar personas ni objetos importantes", "Muda o tamanho sem achatar pessoas ou objetos importantes"),
    h("edit.puppetWarp", "Pin points and bend the layer around them", "Pon alfileres y dobla la capa alrededor de ellos", "Coloque alfinetes e dobre a camada ao redor deles"),
    h("edit.perspectiveWarp", "Change the perspective of a part of the image", "Cambia la perspectiva de una parte de la imagen", "Muda a perspectiva de uma parte da imagem"),
    h("edit.freeTransform", "Scale, rotate and move the layer or selection", "Escala, gira y mueve la capa o la selección", "Escala, gira e move a camada ou a seleção"),
    h("edit.transform.warp", "Bend the layer with a grid of handles", "Deforma la capa con una cuadrícula de controles", "Deforma a camada com uma grade de alças"),
    h("edit.transform.perspective", "Pull corners to change the perspective", "Tira de las esquinas para cambiar la perspectiva", "Puxe os cantos para mudar a perspectiva"),
    h("edit.transform.distort", "Pull each corner freely", "Tira de cada esquina libremente", "Puxe cada canto livremente"),
    h("edit.transform.flipHorizontal", "Mirror the layer left to right", "Voltea la capa de izquierda a derecha", "Espelha a camada da esquerda para a direita"),
    h("edit.transform.flipVertical", "Turn the layer upside down", "Voltea la capa de arriba abajo", "Vira a camada de cabeça para baixo"),
    h("edit.transform.rotate90Cw", "Turn the layer a quarter to the right", "Gira la capa un cuarto a la derecha", "Gira a camada um quarto para a direita"),
    h("edit.autoAlignLayers", "Line up similar layers by their content", "Alinea capas parecidas según su contenido", "Alinha camadas parecidas pelo conteúdo"),
    h("edit.definePattern", "Save the selection as a pattern to fill with", "Guarda la selección como un patrón de relleno", "Salva a seleção como padrão de preenchimento"),
    h("edit.defineBrushPreset", "Turn the selection into a brush", "Convierte la selección en un pincel", "Transforma a seleção num pincel"),
    h("edit.keyboardShortcuts", "See and change the keyboard shortcuts", "Ve y cambia los atajos de teclado", "Veja e mude os atalhos de teclado"),
    h("edit.preferences.general", "Change how the app works: language, interface, saving, performance", "Cambia cómo funciona la app: idioma, interfaz, guardado, rendimiento", "Muda como o app funciona: idioma, interface, salvamento, desempenho"),
    h("edit.purge.all", "Free memory by clearing history and the clipboard", "Libera memoria borrando el historial y el portapapeles", "Libera memória limpando o histórico e a área de transferência"),
    // ---------------------------------------------------------------- Image
    h("image.imageSize", "Change the image's pixel size or resolution", "Cambia el tamaño en píxeles o la resolución de la imagen", "Muda o tamanho em pixels ou a resolução da imagem"),
    h("image.canvasSize", "Add or cut room around the image", "Añade o quita espacio alrededor de la imagen", "Adiciona ou corta espaço ao redor da imagem"),
    h("image.crop", "Crop the image to the selection", "Recorta la imagen a la selección", "Corta a imagem na seleção"),
    h("image.trim", "Cut away empty or single-color borders", "Quita los bordes vacíos o de un solo color", "Tira as bordas vazias ou de uma só cor"),
    h("image.imageRotation.90cw", "Turn the whole image a quarter to the right", "Gira toda la imagen un cuarto a la derecha", "Gira a imagem inteira um quarto para a direita"),
    h("image.imageRotation.90ccw", "Turn the whole image a quarter to the left", "Gira toda la imagen un cuarto a la izquierda", "Gira a imagem inteira um quarto para a esquerda"),
    h("image.imageRotation.180", "Turn the whole image upside down", "Gira toda la imagen media vuelta", "Gira a imagem inteira meia volta"),
    h("image.rotation.arbitrary", "Turn the whole image by any angle", "Gira toda la imagen el ángulo que quieras", "Gira a imagem inteira no ângulo que quiser"),
    h("image.imageRotation.flipCanvasHorizontal", "Mirror the whole image left to right", "Voltea toda la imagen de izquierda a derecha", "Espelha a imagem inteira da esquerda para a direita"),
    h("image.imageRotation.flipCanvasVertical", "Flip the whole image upside down", "Voltea toda la imagen de arriba abajo", "Vira a imagem inteira de cabeça para baixo"),
    h("image.duplicate", "Make a copy of the document", "Hace una copia del documento", "Faz uma cópia do documento"),
    h("image.autoTone", "Fix dark or washed-out images in one step", "Corrige imágenes oscuras o lavadas en un paso", "Corrige imagens escuras ou desbotadas em um passo"),
    h("image.autoContrast", "Stretch the contrast automatically", "Ajusta el contraste automáticamente", "Ajusta o contraste automaticamente"),
    h("image.autoColor", "Remove color casts automatically", "Quita dominantes de color automáticamente", "Remove dominantes de cor automaticamente"),
    h("image.mode.grayscale", "Turn the image into black, white and grays", "Convierte la imagen a blanco, negro y grises", "Converte a imagem para preto, branco e cinzas"),
    h("image.mode.rgb", "Use full color (RGB)", "Usa color completo (RGB)", "Usa cor completa (RGB)"),
    h("image.mode.bitmap", "Only pure black and white pixels (line art, screentone)", "Solo píxeles negros y blancos puros (lineart, tramas)", "Só pixels pretos e brancos puros (line art, retícula)"),
    h("image.adjustments.brightnessContrast", "Make the image lighter, darker or punchier", "Aclara, oscurece o da más contraste a la imagen", "Clareia, escurece ou dá mais contraste à imagem"),
    h("image.adjustments.levels", "Set the black, white and middle points of the image", "Ajusta los puntos de negro, blanco y medios de la imagen", "Ajusta os pontos de preto, branco e meios-tons da imagem"),
    h("image.adjustments.curves", "Fine-tune brightness and contrast with a curve", "Ajusta con precisión brillo y contraste con una curva", "Ajusta com precisão brilho e contraste com uma curva"),
    h("image.adjustments.exposure", "Correct over- or underexposed photos", "Corrige fotos sobreexpuestas o subexpuestas", "Corrige fotos superexpostas ou subexpostas"),
    h("image.adjustments.vibrance", "Make dull colors livelier, gently", "Aviva los colores apagados con suavidad", "Deixa as cores apagadas mais vivas, com suavidade"),
    h("image.adjustments.hueSaturation", "Change the colors, their intensity and lightness", "Cambia los colores, su intensidad y luminosidad", "Muda as cores, a intensidade e a luminosidade"),
    h("image.adjustments.colorBalance", "Warm up or cool down shadows, midtones and highlights", "Calienta o enfría sombras, medios tonos e iluminaciones", "Esquenta ou esfria sombras, meios-tons e realces"),
    h("image.adjustments.blackWhite", "Turn to black and white, choosing how each color looks", "Pasa a blanco y negro eligiendo cómo queda cada color", "Passa para preto e branco escolhendo como fica cada cor"),
    h("image.adjustments.photoFilter", "Tint the image as a colored lens filter would", "Tiñe la imagen como un filtro de color de cámara", "Tinge a imagem como um filtro de cor de câmera"),
    h("image.adjustments.channelMixer", "Mix the red, green and blue channels", "Mezcla los canales rojo, verde y azul", "Mistura os canais vermelho, verde e azul"),
    h("image.adjustments.invert", "Turn the image into its negative", "Convierte la imagen en su negativo", "Transforma a imagem no negativo dela"),
    h("image.adjustments.posterize", "Reduce the image to a few flat tones", "Reduce la imagen a pocos tonos planos", "Reduz a imagem a poucos tons chapados"),
    h("image.adjustments.threshold", "Make every pixel pure black or white", "Hace cada píxel negro o blanco puro", "Deixa cada pixel preto ou branco puro"),
    h("image.adjustments.gradientMap", "Color the image by its lightness with a gradient", "Colorea la imagen según su luminosidad con un degradado", "Colore a imagem pela luminosidade com um degradê"),
    h("image.adjustments.selectiveColor", "Adjust one color range without touching the others", "Ajusta una gama de color sin tocar las demás", "Ajusta uma faixa de cor sem mexer nas outras"),
    h("image.adjustments.shadowsHighlights", "Recover detail in dark shadows and bright highlights", "Recupera detalle en sombras oscuras y luces brillantes", "Recupera detalhes em sombras escuras e realces fortes"),
    h("image.adjustments.desaturate", "Remove all color, keeping the lightness", "Quita todo el color y conserva la luminosidad", "Tira toda a cor e mantém a luminosidade"),
    h("image.adjustments.replaceColor", "Pick a color in the image and swap it for another", "Elige un color de la imagen y cámbialo por otro", "Escolha uma cor da imagem e troque por outra"),
    h("image.adjustments.matchColor", "Make one image's colors match another's", "Iguala los colores de una imagen con los de otra", "Iguala as cores de uma imagem às de outra"),
    // ---------------------------------------------------------------- Layer
    h("layer.new.layer", "Add a new empty layer", "Añade una capa vacía nueva", "Adiciona uma camada vazia nova"),
    h("layer.new.group", "Add a group (a folder) for layers", "Añade un grupo (una carpeta) para capas", "Adiciona um grupo (uma pasta) para camadas"),
    h("layer.new.layerViaCopy", "Copy the selection onto a new layer", "Copia la selección a una capa nueva", "Copia a seleção para uma camada nova"),
    h("layer.new.layerViaCut", "Move the selection onto a new layer", "Mueve la selección a una capa nueva", "Move a seleção para uma camada nova"),
    h("layer.duplicate", "Make a copy of the layer", "Hace una copia de la capa", "Faz uma cópia da camada"),
    h("layer.delete", "Delete the selected layers", "Elimina las capas seleccionadas", "Apaga as camadas selecionadas"),
    h("layer.renameLayer", "Give the layer a new name", "Cambia el nombre de la capa", "Muda o nome da camada"),
    h("layer.quickExportAsPng", "Save just this layer as a PNG", "Guarda solo esta capa como PNG", "Salva só esta camada como PNG"),
    h("layer.layerStyle.blendingOptions", "Effects like shadow, stroke and glow for the layer", "Efectos como sombra, trazo y resplandor para la capa", "Efeitos como sombra, contorno e brilho para a camada"),
    h("layer.layerStyle.stroke", "Add an outline around the layer's content (text borders)", "Añade un contorno alrededor del contenido (borde del texto)", "Adiciona um contorno ao redor do conteúdo (borda do texto)"),
    h("layer.layerStyle.dropShadow", "Add a shadow behind the layer", "Añade una sombra detrás de la capa", "Adiciona uma sombra atrás da camada"),
    h("layer.layerStyle.outerGlow", "Add a glow around the layer", "Añade un resplandor alrededor de la capa", "Adiciona um brilho ao redor da camada"),
    h("layer.layerStyle.colorOverlay", "Paint the whole layer one color, keeping its shape", "Pinta toda la capa de un color conservando su forma", "Pinta a camada inteira de uma cor mantendo a forma"),
    h("layer.layerStyle.copyLayerStyle", "Copy the layer's effects to use elsewhere", "Copia los efectos de la capa para usarlos en otra", "Copia os efeitos da camada para usar em outra"),
    h("layer.layerStyle.pasteLayerStyle", "Give the selected layers the copied effects", "Aplica los efectos copiados a las capas seleccionadas", "Aplica os efeitos copiados às camadas selecionadas"),
    h("layer.layerStyle.clear", "Take every effect off the layer", "Quita todos los efectos de la capa", "Tira todos os efeitos da camada"),
    h("layer.newFillLayer.solidColor", "Add a layer of one color that you can change later", "Añade una capa de un color que puedes cambiar después", "Adiciona uma camada de uma cor que dá para mudar depois"),
    h("layer.newAdjustmentLayer.levels", "Levels on its own layer, editable at any time", "Niveles en su propia capa, editable cuando quieras", "Níveis em camada própria, editável quando quiser"),
    h("layer.newAdjustmentLayer.hueSaturation", "Hue/Saturation on its own layer, editable at any time", "Tono/saturación en su propia capa, editable cuando quieras", "Matiz/saturação em camada própria, editável quando quiser"),
    h("layer.layerMask.revealAll", "Add a mask that shows the whole layer, to hide parts later", "Añade una máscara que muestra toda la capa para ocultar partes después", "Adiciona uma máscara que mostra a camada inteira para esconder partes depois"),
    h("layer.layerMask.hideAll", "Add a mask that hides the whole layer", "Añade una máscara que oculta toda la capa", "Adiciona uma máscara que esconde a camada inteira"),
    h("layer.layerMask.revealSelection", "Add a mask that shows only the selection", "Añade una máscara que solo muestra la selección", "Adiciona uma máscara que só mostra a seleção"),
    h("layer.layerMask.apply", "Make the mask permanent on the layer's pixels", "Aplica la máscara a los píxeles de la capa para siempre", "Aplica a máscara aos pixels da camada de vez"),
    h("layer.createClippingMask", "Show this layer only inside the layer below", "Muestra esta capa solo dentro de la de abajo", "Mostra esta camada só dentro da de baixo"),
    h("layer.smartObjects.convertToSmartObject", "Wrap the layer so you can scale it without losing quality", "Envuelve la capa para escalarla sin perder calidad", "Envolve a camada para escalar sem perder qualidade"),
    h("layer.rasterize.layer", "Turn text, shapes or smart objects into plain pixels", "Convierte texto, formas u objetos inteligentes en píxeles", "Converte texto, formas ou objetos inteligentes em pixels"),
    h("layer.rasterize.type", "Turn the text into plain pixels (no longer editable)", "Convierte el texto en píxeles (ya no se podrá editar)", "Converte o texto em pixels (não dá mais para editar)"),
    h("layer.groupLayers", "Put the selected layers in a group", "Mete las capas seleccionadas en un grupo", "Coloca as camadas selecionadas num grupo"),
    h("layer.ungroupLayers", "Take the layers out of their group", "Saca las capas de su grupo", "Tira as camadas do grupo"),
    h("layer.hideLayers", "Hide the selected layers", "Oculta las capas seleccionadas", "Esconde as camadas selecionadas"),
    h("layer.arrange.bringToFront", "Move the layer to the top of the stack", "Sube la capa arriba del todo", "Leva a camada para o topo"),
    h("layer.arrange.sendToBack", "Move the layer to the bottom of the stack", "Baja la capa al fondo", "Leva a camada para o fundo"),
    h("layer.align.horizontalCenters", "Center the selected layers on each other, left to right", "Centra las capas seleccionadas entre sí en horizontal", "Centraliza as camadas selecionadas entre si na horizontal"),
    h("layer.lockLayers", "Lock layers so they can't be changed by accident", "Bloquea capas para no cambiarlas sin querer", "Bloqueia camadas para não mudá-las sem querer"),
    h("layer.mergeLayers", "Join the selected layers into one", "Une las capas seleccionadas en una", "Junta as camadas selecionadas em uma"),
    h("layer.mergeVisible", "Join every visible layer into one", "Une todas las capas visibles en una", "Junta todas as camadas visíveis em uma"),
    h("layer.flattenImage", "Merge all layers into a single background", "Une todas las capas en un solo fondo", "Junta todas as camadas num só fundo"),
    // ---------------------------------------------------------------- Type
    h("type.warpText", "Bend text into an arc, a wave, a flag…", "Deforma el texto en arco, onda, bandera…", "Deforma o texto em arco, onda, bandeira…"),
    h("type.panels.character", "Font, size, spacing and color of the text", "Fuente, tamaño, espaciado y color del texto", "Fonte, tamanho, espaçamento e cor do texto"),
    h("type.panels.paragraph", "Alignment, indents and spacing of paragraphs", "Alineación, sangrías y espacios de los párrafos", "Alinhamento, recuos e espaços dos parágrafos"),
    h("type.convertToShape", "Turn text into an editable shape", "Convierte el texto en una forma editable", "Converte o texto numa forma editável"),
    h("type.resolveMissingFonts", "Find or replace fonts this device doesn't have", "Busca o reemplaza las fuentes que faltan en este dispositivo", "Procura ou substitui as fontes que faltam neste dispositivo"),
    // ---------------------------------------------------------------- Select
    h("select.all", "Select the whole canvas", "Selecciona todo el lienzo", "Seleciona a tela inteira"),
    h("select.deselect", "Drop the selection", "Quita la selección", "Tira a seleção"),
    h("select.reselect", "Bring back the last selection", "Recupera la última selección", "Recupera a última seleção"),
    h("select.inverse", "Select everything that was not selected", "Selecciona todo lo que no estaba seleccionado", "Seleciona tudo o que não estava selecionado"),
    h("select.colorRange", "Select every pixel of a color you pick", "Selecciona todos los píxeles de un color que elijas", "Seleciona todos os pixels de uma cor escolhida"),
    h("select.subject", "Select the main subject of the image automatically", "Selecciona automáticamente el sujeto principal de la imagen", "Seleciona automaticamente o objeto principal da imagem"),
    h("select.sky", "Select the sky in a photo automatically", "Selecciona automáticamente el cielo de una foto", "Seleciona automaticamente o céu de uma foto"),
    h("select.focusArea", "Select what is in focus in a photo", "Selecciona lo que está enfocado en una foto", "Seleciona o que está em foco numa foto"),
    h("select.selectAndMask", "Refine the selection's edges (hair, fur, soft edges)", "Refina los bordes de la selección (pelo, bordes suaves)", "Refina as bordas da seleção (cabelo, bordas suaves)"),
    h("select.modify.expand", "Grow the selection by some pixels", "Agranda la selección unos píxeles", "Aumenta a seleção alguns pixels"),
    h("select.modify.contract", "Shrink the selection by some pixels", "Encoge la selección unos píxeles", "Diminui a seleção alguns pixels"),
    h("select.modify.feather", "Soften the selection's edge", "Suaviza el borde de la selección", "Suaviza a borda da seleção"),
    h("select.modify.border", "Select a band along the selection's edge", "Selecciona una franja por el borde de la selección", "Seleciona uma faixa na borda da seleção"),
    h("select.modify.smooth", "Round off the selection's jagged corners", "Redondea los picos de la selección", "Arredonda os bicos da seleção"),
    h("select.grow", "Add neighbouring pixels of similar color", "Añade píxeles vecinos de color parecido", "Adiciona pixels vizinhos de cor parecida"),
    h("select.similar", "Add every pixel of similar color in the image", "Añade todos los píxeles de color parecido de la imagen", "Adiciona todos os pixels de cor parecida da imagem"),
    h("select.transformSelection", "Scale or rotate the selection outline, not the pixels", "Escala o gira el contorno de la selección, no los píxeles", "Escala ou gira o contorno da seleção, não os pixels"),
    h("select.editInQuickMaskMode", "Paint the selection with the brush", "Pinta la selección con el pincel", "Pinte a seleção com o pincel"),
    h("select.saveSelection", "Keep the selection to load it again later", "Guarda la selección para cargarla más tarde", "Guarda a seleção para carregar depois"),
    h("select.loadSelection", "Bring back a saved selection", "Recupera una selección guardada", "Recupera uma seleção salva"),
    // ---------------------------------------------------------------- Filter
    h("filter.lastFilter", "Run the last filter again with the same settings", "Repite el último filtro con los mismos ajustes", "Repete o último filtro com os mesmos ajustes"),
    h("filter.filterGallery", "Try artistic filters with a big preview", "Prueba filtros artísticos con una vista previa grande", "Experimente filtros artísticos com uma prévia grande"),
    h("filter.cameraRaw", "Edit light, color and detail like a photo app", "Edita luz, color y detalle como en una app de fotos", "Edite luz, cor e detalhe como num app de fotos"),
    h("filter.lensCorrection", "Fix lens distortion and tilted photos", "Corrige la distorsión del objetivo y fotos torcidas", "Corrige a distorção da lente e fotos tortas"),
    h("filter.liquify", "Push, pull and bloat parts of the image", "Empuja, estira e infla partes de la imagen", "Empurre, puxe e infle partes da imagem"),
    h("filter.blur.gaussianBlur", "Blur the image evenly", "Desenfoca la imagen de forma uniforme", "Desfoca a imagem por igual"),
    h("filter.blur.motionBlur", "Blur in one direction, like fast movement", "Desenfoca en una dirección, como un movimiento rápido", "Desfoca numa direção, como movimento rápido"),
    h("filter.blur.radialBlur", "Spin or zoom blur from a center (speed lines)", "Desenfoque que gira o se aleja del centro (líneas de velocidad)", "Desfoque que gira ou se afasta do centro (linhas de velocidade)"),
    h("filter.blur.surfaceBlur", "Smooth surfaces but keep the edges sharp", "Suaviza superficies y conserva nítidos los bordes", "Suaviza superfícies e mantém as bordas nítidas"),
    h("filter.sharpen.unsharpMask", "Sharpen edges, with control over how much", "Enfoca los bordes controlando cuánto", "Aumenta a nitidez das bordas controlando quanto"),
    h("filter.sharpen.smartSharpen", "Sharpen while keeping noise down", "Enfoca evitando que aparezca ruido", "Aumenta a nitidez evitando ruído"),
    h("filter.noise.addNoise", "Add grain to the image", "Añade grano a la imagen", "Adiciona granulação à imagem"),
    h("filter.noise.reduceNoise", "Clean up grain and color noise", "Limpia el grano y el ruido de color", "Limpa a granulação e o ruído de cor"),
    h("filter.noise.median", "Smooth out noise and specks; also removes screentone dots", "Suaviza ruido y motas; también quita puntos de trama", "Suaviza ruído e pontinhos; também tira pontos de retícula"),
    h("filter.noise.dustAndScratches", "Remove dust and small scratches from scans", "Quita polvo y rayitas de los escaneos", "Remove poeira e riscos pequenos dos scans"),
    h("filter.noise.despeckle", "Remove fine specks while keeping edges", "Quita motas finas conservando los bordes", "Remove pontinhos finos mantendo as bordas"),
    h("filter.other.highPass", "Keep only edges and details (for sharpening)", "Deja solo bordes y detalles (para enfocar)", "Deixa só bordas e detalhes (para dar nitidez)"),
    h("filter.other.minimum", "Thicken dark lines and shrink light areas", "Engrosa las líneas oscuras y encoge las zonas claras", "Engrossa as linhas escuras e encolhe as áreas claras"),
    h("filter.other.maximum", "Thin dark lines and grow light areas", "Adelgaza las líneas oscuras y agranda las zonas claras", "Afina as linhas escuras e aumenta as áreas claras"),
    h("filter.other.offset", "Shift the image, wrapping it around the edges", "Desplaza la imagen dando la vuelta por los bordes", "Desloca a imagem dando a volta pelas bordas"),
    h("filter.pixelate.colorHalftone", "Turn the image into a comic-style dot pattern", "Convierte la imagen en puntos estilo cómic", "Transforma a imagem em pontos estilo quadrinho"),
    h("filter.pixelate.mosaic", "Turn into big square pixels (to censor)", "Convierte en píxeles grandes (para censurar)", "Transforma em pixels grandes (para censurar)"),
    h("filter.stylize.findEdges", "Draw the outlines of the image", "Dibuja los contornos de la imagen", "Desenha os contornos da imagem"),
    h("filter.render.clouds", "Fill with soft random clouds", "Rellena con nubes suaves al azar", "Preenche com nuvens suaves aleatórias"),
    // ---------------------------------------------------------------- View
    h("view.zoomIn", "Get closer to the image", "Acerca la imagen", "Aproxima a imagem"),
    h("view.zoomOut", "See more of the image", "Aleja la imagen", "Afasta a imagem"),
    h("view.fitOnScreen", "Show the whole image on the screen", "Muestra toda la imagen en la pantalla", "Mostra a imagem inteira na tela"),
    h("view.actualPixels", "Show the image at 100%, one pixel per pixel", "Muestra la imagen al 100 %, píxel a píxel", "Mostra a imagem em 100 %, pixel a pixel"),
    h("view.rulers", "Show or hide the rulers", "Muestra u oculta las reglas", "Mostra ou esconde as réguas"),
    h("view.show.grid", "Show or hide a grid over the image", "Muestra u oculta una cuadrícula sobre la imagen", "Mostra ou esconde uma grade sobre a imagem"),
    h("view.show.guides", "Show or hide the guides", "Muestra u oculta las guías", "Mostra ou esconde as guias"),
    h("view.newGuide", "Add a guide line at an exact position", "Añade una guía en una posición exacta", "Adiciona uma guia numa posição exata"),
    h("view.snap", "Make moved things stick to guides and edges", "Hace que lo que mueves se pegue a guías y bordes", "Faz o que você move grudar em guias e bordas"),
    h("view.extras", "Show or hide guides, grid and selection edges at once", "Muestra u oculta a la vez guías, cuadrícula y bordes de selección", "Mostra ou esconde de uma vez guias, grade e bordas da seleção"),
    h("view.simpleUi", "Switch between the simple interface and the full one", "Cambia entre la interfaz simple y la completa", "Alterna entre a interface simples e a completa"),
    // ---------------------------------------------------------------- Window (panels)
    h("window.panel.layers", "See, order, hide and select the layers", "Ve, ordena, oculta y selecciona las capas", "Veja, ordene, esconda e selecione as camadas"),
    h("window.panel.history", "Go back to any earlier step", "Vuelve a cualquier paso anterior", "Volta para qualquer passo anterior"),
    h("window.panel.color", "Pick the colors to paint with", "Elige los colores para pintar", "Escolha as cores para pintar"),
    h("window.panel.swatches", "Saved colors, one tap away", "Colores guardados a un toque", "Cores salvas a um toque"),
    h("window.panel.brushes", "Choose a brush tip", "Elige la punta del pincel", "Escolha a ponta do pincel"),
    h("window.panel.brushSettings", "Shape, spacing and pressure of the brush", "Forma, espaciado y presión del pincel", "Forma, espaçamento e pressão do pincel"),
    h("window.panel.properties", "Settings of the selected layer, mask or text", "Ajustes de la capa, máscara o texto seleccionado", "Ajustes da camada, máscara ou texto selecionado"),
    h("window.panel.adjustments", "Add adjustment layers with one tap", "Añade capas de ajuste con un toque", "Adiciona camadas de ajuste com um toque"),
    h("window.panel.character", "Font, size, spacing and color of the text", "Fuente, tamaño, espaciado y color del texto", "Fonte, tamanho, espaçamento e cor do texto"),
    h("window.panel.paragraph", "Alignment, indents and spacing of paragraphs", "Alineación, sangrías y espacios de los párrafos", "Alinhamento, recuos e espaços dos parágrafos"),
    h("window.panel.channels", "See the red, green and blue channels and saved selections", "Ve los canales rojo, verde y azul y las selecciones guardadas", "Veja os canais vermelho, verde e azul e as seleções salvas"),
    h("window.panel.navigator", "A small map of the whole image to move around", "Un mapa pequeño de toda la imagen para moverte", "Um mapa pequeno da imagem inteira para se mover"),
    h("window.panel.info", "Color values and position under the pointer", "Valores de color y posición bajo el puntero", "Valores de cor e posição sob o ponteiro"),
    h("window.panel.histogram", "A chart of how light or dark the image is", "Un gráfico de cuán clara u oscura es la imagen", "Um gráfico de quão clara ou escura é a imagem"),
    h("window.panel.actions", "Record steps and play them again", "Graba pasos y repítelos", "Grave passos e repita"),
    h("window.panel.typer", "Paste a script and place its lines in the bubbles", "Pega un guion y coloca sus líneas en los globos", "Cole um roteiro e coloque as falas nos balões"),
    h("window.panel.glyphs", "Insert special characters and symbols", "Inserta caracteres especiales y símbolos", "Insere caracteres especiais e símbolos"),
    h("window.panel.styles", "Ready-made layer effects", "Efectos de capa ya hechos", "Efeitos de camada prontos"),
    h("window.panel.patterns", "Patterns to fill with", "Patrones para rellenar", "Padrões para preencher"),
    h("window.panel.gradients", "Gradients to fill with", "Degradados para rellenar", "Degradês para preencher"),
    h("window.panel.paths", "Saved paths from the Pen tool", "Trazados guardados de la Pluma", "Traçados salvos da Caneta"),
];

/// The sentence for `id` in `lang`.
pub fn describe(id: &str, lang: Lang) -> Option<&'static str> {
    HELP.iter().find(|h| h.id == id).map(|h| h.text(lang))
}

/// The entry for `id`.
pub fn entry(id: &str) -> Option<&'static Help> {
    HELP.iter().find(|h| h.id == id)
}

/// The palette's result groups, in their order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    Tools,
    Commands,
    Panels,
    Ai,
}

impl Group {
    /// The group's heading (a `tl!` literal).
    pub fn title(self) -> &'static str {
        match self {
            Group::Tools => tl!("Tools"),
            Group::Commands => tl!("Commands"),
            Group::Panels => tl!("Panels"),
            Group::Ai => tl!("AI"),
        }
    }
}

/// Which group a palette row (`tool:Name`, a command id) goes in.
pub fn group_of(id: &str) -> Group {
    if matches!(id, "tool:RemoveAi" | "edit.inpaintAi" | "edit.generativeFill" | "edit.removeAi" | "image.upscaleAi" | "window.aiModels") {
        Group::Ai
    } else if id.starts_with("tool:") {
        Group::Tools
    } else if id.starts_with("window.panel.") || id.starts_with("window.toggle.") || id.starts_with("type.panels.") {
        Group::Panels
    } else {
        Group::Commands
    }
}

/// Tools and commands that do similar jobs, and how to choose between them.
pub struct Which {
    pub members: &'static [&'static str],
    pub en: &'static str,
    pub es: &'static str,
    pub pt: &'static str,
}

impl Which {
    pub fn text(&self, lang: Lang) -> &'static str {
        match lang_key(lang) {
            "es" => self.es,
            "pt" => self.pt,
            _ => self.en,
        }
    }
}

#[rustfmt::skip]
pub const WHICH: &[Which] = &[
    Which {
        members: &["tool:RemoveAi", "edit.inpaintAi", "tool:Remove", "tool:SpotHealing", "tool:Healing", "tool:Patch", "tool:CloneStamp", "edit.contentAwareFill"],
        en: "Remove with AI: paint over text or objects, the best result. Clean with AI: the same on a selection. Remove Tool: no AI, fast, for plain backgrounds. Spot Healing: small spots and specks.",
        es: "Quitar con IA: pinta sobre textos u objetos, el mejor resultado. Limpiar con IA: lo mismo sobre una selección. Herramienta Quitar: sin IA, rápida, para fondos simples. Pincel corrector puntual: manchas y motas pequeñas.",
        pt: "Remover com IA: pinte sobre textos ou objetos, o melhor resultado. Limpar com IA: o mesmo sobre uma seleção. Ferramenta Remover: sem IA, rápida, para fundos simples. Pincel de recuperação para manchas: manchas e pontinhos pequenos.",
    },
    Which {
        members: &["tool:QuickSelection", "tool:MagicWand", "tool:ObjectSelection", "select.subject", "tool:Lasso", "select.colorRange"],
        en: "Quick Selection: paint over the object. Magic Wand: one flat color. Object Selection: a box around it. Lasso: by hand.",
        es: "Selección rápida: pinta sobre el objeto. Varita mágica: un color plano. Selección de objetos: un recuadro alrededor. Lazo: a mano.",
        pt: "Seleção rápida: pinte sobre o objeto. Varinha mágica: uma cor chapada. Seleção de objeto: um retângulo ao redor. Laço: à mão.",
    },
    Which {
        members: &["tool:Eraser", "tool:MagicEraser", "tool:BackgroundEraser"],
        en: "Eraser: erase where you paint. Magic Eraser: erase one color with a tap. Background Eraser: erase around an edge and keep the subject.",
        es: "Borrador: borra donde pintas. Borrador mágico: borra un color con un toque. Borrador de fondos: borra junto a un borde y conserva el sujeto.",
        pt: "Borracha: apaga onde você pinta. Borracha mágica: apaga uma cor com um toque. Borracha de plano de fundo: apaga junto a uma borda e mantém o objeto.",
    },
    Which {
        members: &["tool:BubbleType", "tool:Type", "tool:VerticalType"],
        en: "Bubble Type: tap a bubble and the text fits inside. Horizontal Type: write where you tap. Vertical Type: top to bottom.",
        es: "Texto en globo: toca un globo y el texto se ajusta solo. Texto horizontal: escribe donde tocas. Texto vertical: de arriba abajo.",
        pt: "Texto no balão: toque um balão e o texto se ajusta sozinho. Texto horizontal: escreva onde tocar. Texto vertical: de cima para baixo.",
    },
    Which {
        members: &["image.upscaleAi", "image.imageSize"],
        en: "Upscale with AI: bigger with new detail (slower). Image Size: changes the pixels without AI (fast).",
        es: "Escalar con IA: más grande y con detalle nuevo (más lento). Tamaño de imagen: cambia los píxeles sin IA (rápido).",
        pt: "Ampliar com IA: maior e com detalhe novo (mais lento). Tamanho da imagem: muda os pixels sem IA (rápido).",
    },
];

/// The hint for the first family two or more of `ids` belong to.
pub fn which_for<'a>(ids: impl IntoIterator<Item = &'a str>) -> Option<&'static Which> {
    let ids: Vec<&str> = ids.into_iter().collect();
    WHICH.iter().find(|w| ids.iter().filter(|id| w.members.contains(id)).count() >= 2)
}

/// How well the words of `query` match `text` (a description): every query word of three
/// letters or more must start a word of the text, or share its stem ("borrar" and "borra");
/// `None` otherwise. Accents and case don't matter.
pub fn words_score(query: &str, text: &str) -> Option<i32> {
    let q = crate::palette_search::fold(query);
    let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).collect();
    if words.is_empty() {
        return None;
    }
    let t = crate::palette_search::fold(text);
    let tw: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut score = 0;
    for w in &words {
        let best = tw.iter().filter_map(|x| stem_match(w, x)).max()?;
        score += best;
    }
    Some(score)
}

/// `q` matches word `w` when one starts the other, or they share all but the last letter of
/// the shorter (four letters or more): "borrar"/"borra", "texto"/"textos", "quitar"/"quita".
fn stem_match(q: &str, w: &str) -> Option<i32> {
    if w.starts_with(q) {
        return Some(if w == q { 3 } else { 2 });
    }
    let common = q.chars().zip(w.chars()).take_while(|(a, b)| a == b).count();
    let shorter = q.chars().count().min(w.chars().count());
    (common >= 4 && common + 1 >= shorter).then_some(1)
}

#[cfg(test)]
#[path = "help_text_tests.rs"]
mod tests;
